"""
title: rpi UI Adapter
author: pi-rust
version: 0.1.0
required_open_webui_version: 0.12.2
description: rpi-ui-adapter managed function; native ACP events for Open WebUI.
"""

import asyncio
import contextlib
import hashlib
import html
import json
import os
from pathlib import Path
import re
import sqlite3
import time
import uuid

import aiohttp
from pydantic import BaseModel, Field


class AcpError(RuntimeError):
    pass


class AcpClient:
    """Bidirectional ACP RPC; the reader always handles prompt completion,
    including when no notification follows the final text/tool update.
    """

    def __init__(self, socket):
        self.socket = socket
        self.pending = {}
        self.events = asyncio.Queue(maxsize=2048)
        self.sequence = 0
        self.session_id = None
        self.reader = asyncio.create_task(self._read())

    async def _read(self):
        error = AcpError("ACP connection closed before completion")
        try:
            async for frame in self.socket:
                if frame.type != aiohttp.WSMsgType.TEXT:
                    break
                message = json.loads(frame.data)
                if not isinstance(message, dict):
                    raise AcpError("Invalid ACP frame")
                if "method" in message:
                    self.events.put_nowait(message)
                elif message.get("id") in self.pending:
                    future = self.pending.pop(message["id"])
                    if not future.done():
                        if "error" in message:
                            future.set_exception(AcpError(str(message["error"].get("message", "ACP request failed"))))
                        else:
                            future.set_result(message.get("result", {}))
        except asyncio.CancelledError:
            pass
        except Exception as exc:
            error = AcpError(f"ACP transport failed: {type(exc).__name__}")
        finally:
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(error)
            self.pending.clear()

    async def begin(self, method, params):
        if self.reader.done():
            raise AcpError("ACP connection is closed")
        self.sequence += 1
        future = asyncio.get_running_loop().create_future()
        self.pending[self.sequence] = future
        try:
            await self.socket.send_json({"jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params})
        except BaseException:
            self.pending.pop(self.sequence, None)
            future.cancel()
            raise
        return future

    async def request(self, method, params):
        return await asyncio.wait_for(await self.begin(method, params), 30)

    async def notify(self, method, params):
        await self.socket.send_json({"jsonrpc": "2.0", "method": method, "params": params})

    async def reply(self, request_id, result=None, error=None):
        message = {"jsonrpc": "2.0", "id": request_id}
        message["error" if error else "result"] = error if error else result
        await self.socket.send_json(message)

    async def updates(self, completion):
        try:
            while not completion.done() or not self.events.empty():
                if not self.events.empty():
                    yield self.events.get_nowait()
                    continue
                receive = asyncio.create_task(self.events.get())
                try:
                    done, _ = await asyncio.wait([receive, completion], return_when=asyncio.FIRST_COMPLETED)
                    if receive in done:
                        yield receive.result()
                finally:
                    if not receive.done():
                        receive.cancel()
                    with contextlib.suppress(asyncio.CancelledError):
                        await receive
        finally:
            if not completion.done() and self.session_id:
                with contextlib.suppress(Exception):
                    await asyncio.wait_for(self.notify("session/cancel", {"sessionId": self.session_id}), 2)
            # Retrieve errors even when iteration was cancelled before the caller
            # awaited the prompt response; never leave unobserved futures.
            if completion.done() and not completion.cancelled():
                completion.exception()

    async def close(self):
        await self.socket.close()
        self.reader.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await self.reader


def canonical(messages):
    result = []
    for message in messages:
        role, content = message.get("role"), message.get("content", "")
        if role not in ("system", "user", "assistant"):
            raise ValueError("UI Adapter accepts text conversations; disable Open WebUI tool execution for this model")
        if isinstance(content, list):
            if any(part.get("type") != "text" for part in content):
                raise ValueError("This ACP adapter currently supports text input only")
            content = "\n".join(part.get("text", "") for part in content)
        if not isinstance(content, str):
            raise ValueError("Invalid message content")
        if role == "assistant":
            content = re.sub(r'<details type="tool_calls" data-adapter="rpi-ui-adapter"[^>]*>.*?</details>', "", content, flags=re.S)
            content = re.sub(r"<think>.*?</think>", "", content, flags=re.S)
        result.append({"role": role, "content": content.strip()})
    return result


def digest(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True).encode()).hexdigest()


@contextlib.asynccontextmanager
async def cancel_before_disconnect(client, session_id):
    # This scope is inside ws_connect. An outer cancellation handler runs only
    # after aiohttp has closed the socket and would send session/cancel too late.
    try:
        yield
    except BaseException:
        with contextlib.suppress(Exception):
            await asyncio.wait_for(client.notify("session/cancel", {"sessionId": session_id}), 2)
        raise


class SessionStore:
    """A lease serializes one chat across backend workers. Only a completed
    turn with an exact transcript prefix can resume a native ACP session.
    """

    def __init__(self, directory):
        directory = Path(directory)
        directory.mkdir(parents=True, exist_ok=True)
        self.path = directory / "sessions.sqlite3"
        with self.connect() as db:
            db.execute("CREATE TABLE IF NOT EXISTS sessions (key TEXT PRIMARY KEY, sid TEXT, fingerprint TEXT, lease TEXT, expires REAL)")

    @contextlib.contextmanager
    def connect(self):
        db = sqlite3.connect(self.path, timeout=10)
        try:
            with db:
                yield db
        finally:
            db.close()

    def claim(self, key, duration):
        lease = uuid.uuid4().hex
        with self.connect() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute("SELECT sid, fingerprint, lease, expires FROM sessions WHERE key=?", (key,)).fetchone()
            if row and row[2] and row[3] > time.time():
                raise ValueError("This chat already has a running request; stop it before retrying")
            db.execute("INSERT OR REPLACE INTO sessions VALUES (?, NULL, NULL, ?, ?)", (key, lease, time.time() + duration + 60))
        return lease, (row[0], row[1]) if row else (None, None)

    def finish(self, key, lease, sid=None, fingerprint=None):
        with self.connect() as db:
            db.execute("UPDATE sessions SET sid=?, fingerprint=?, lease=NULL, expires=0 WHERE key=? AND lease=?", (sid, fingerprint, key, lease))


def tool_detail(tool):
    def attribute(value):
        return html.escape(str(value), quote=True)

    def payload(value):
        encoded = json.dumps(value, ensure_ascii=False)
        if len(encoded) > 32000:
            encoded = json.dumps(encoded[:32000] + "… [truncated]", ensure_ascii=False)
        return attribute(encoded)

    # Native ToolCallDisplay reads these completed details attributes. This is
    # display markup, not choices[].delta.tool_calls (which would execute the
    # tools again). Newlines after details/summary are required by its tokenizer.
    state = "failed" if tool.get("status") == "failed" else "completed"
    return ('\n\n<details type="tool_calls" data-adapter="rpi-ui-adapter" done="true" '
            + 'id="' + attribute(tool.get("toolCallId", "tool")) + '" '
            + 'name="' + attribute(tool.get("name", tool.get("title", "工具"))) + '" '
            + 'arguments="' + payload(tool.get("rawInput")) + '" '
            + 'result="' + payload(tool.get("rawOutput", tool.get("content"))) + '" '
            + 'status="' + state + '">\n<summary>Tool Executed</summary>\n\n</details>\n\n')


class Pipe:
    class Valves(BaseModel):
        ADAPTER_URL: str = Field(default="http://127.0.0.1:5234")
        ADAPTER_TOKEN: str = Field(default="", json_schema_extra={"input": {"type": "password"}})
        CWD: str = Field(default="")
        PERMISSIONS: str = Field(default="ask", description="ask / bypass / deny")
        TIMEOUT_SECONDS: int = Field(default=1800, ge=30, le=86400)
        STATE_DIR: str = Field(default="", description="Persistent session index; defaults to DATA_DIR/ui-adapter")

    def __init__(self):
        self.valves = self.Valves()

    async def permission(self, client, event, event_call, session_id):
        params = event.get("params", {})
        outcome = {"outcome": "cancelled"}
        if params.get("sessionId") == session_id:
            options = params.get("options", [])
            allowed = next((o for o in options if o.get("kind") == "allow_once"), None)
            if allowed is None:
                allowed = next((o for o in options if o.get("kind") == "allow_always"), None)
            approve = self.valves.PERMISSIONS == "bypass"
            if self.valves.PERMISSIONS == "ask" and event_call and allowed:
                tool = params.get("toolCall", {})
                try:
                    answer = await asyncio.wait_for(event_call({"type": "confirmation", "data": {
                        "title": "允许执行工具？", "message": str(tool.get("title", "工具请求")) + "\n" + json.dumps(tool.get("rawInput", {}), ensure_ascii=False),
                    }}), 300)
                    # Timeout/disconnect returns an error dict, which is truthy.
                    # Accept only an actual affirmative boolean.
                    approve = answer is True
                except Exception:
                    approve = False
            if approve and allowed:
                outcome = {"outcome": "selected", "optionId": allowed["optionId"]}
        await client.reply(event["id"], {"outcome": outcome})

    async def pipe(self, body: dict, __user__: dict = None, __chat_id__: str = None,
                   __event_emitter__=None, __event_call__=None, __task__=None):
        # Utility tasks must not create side-effecting agent runs (title, tags,
        # autocomplete etc.). Use a normal LLM for those tasks if desired.
        if __task__:
            yield ""
            return
        if not self.valves.ADAPTER_TOKEN:
            raise ValueError("UI Adapter is not configured; start rpi --webui=openwebui")
        if self.valves.PERMISSIONS not in ("ask", "bypass", "deny"):
            raise ValueError("Invalid permission policy")
        messages = canonical(body.get("messages", []))
        if not messages or messages[-1]["role"] != "user":
            raise ValueError("The last message must be a user request")
        prefix = messages[:-1]
        key = digest([(__user__ or {}).get("id", "api"), __chat_id__ or uuid.uuid4().hex, self.valves.ADAPTER_URL, self.valves.CWD])
        state_dir = self.valves.STATE_DIR or str(Path(os.environ.get("DATA_DIR", "./data")) / "ui-adapter")
        store = await asyncio.to_thread(SessionStore, state_dir)
        lease, (saved_sid, saved_prefix) = await asyncio.to_thread(store.claim, key, self.valves.TIMEOUT_SECONDS)
        sid, completed, client, completion = None, False, None, None
        requests = {}
        rendered = []
        thinking = False
        tools = {}

        def emit(text):
            rendered.append(text)
            return text

        async def status(description, done=False):
            if __event_emitter__:
                await __event_emitter__({"type": "status", "data": {"description": description, "done": done}})

        try:
            await status("正在连接 rpi…")
            base = self.valves.ADAPTER_URL.rstrip("/")
            url = ("wss" + base[5:] if base.startswith("https:") else "ws" + base[4:]) + "/acp"
            timeout = aiohttp.ClientTimeout(total=self.valves.TIMEOUT_SECONDS + 30, connect=15)
            async with aiohttp.ClientSession(timeout=timeout, trust_env=False) as http:
                async with http.ws_connect(url, headers={"Authorization": "Bearer " + self.valves.ADAPTER_TOKEN}, max_msg_size=4 * 1024 * 1024, heartbeat=20) as socket:
                    client = AcpClient(socket)
                    init = await client.request("initialize", {"protocolVersion": 1, "clientInfo": {"name": "rpi-ui-adapter", "version": "0.1.0"}, "clientCapabilities": {}})
                    if init.get("protocolVersion") != 1:
                        raise AcpError("Unsupported ACP protocol version")
                    caps = init.get("agentCapabilities", {})
                    can_resume = "resume" in caps.get("sessionCapabilities", {}) or caps.get("loadSession")
                    if saved_sid and saved_prefix == digest(prefix) and can_resume:
                        method = "session/resume" if "resume" in caps.get("sessionCapabilities", {}) else "session/load"
                        await client.request(method, {"sessionId": saved_sid, "cwd": self.valves.CWD, "mcpServers": []})
                        sid = saved_sid
                        # session/load replays history before its response. It
                        # must never appear in this turn's rendered answer.
                        while not client.events.empty():
                            client.events.get_nowait()
                    if sid is None:
                        result = await client.request("session/new", {"cwd": self.valves.CWD, "mcpServers": []})
                        sid = result["sessionId"]
                    prompt = messages[-1]["content"]
                    if sid != saved_sid and prefix:
                        await status("正在载入当前分支的对话上下文…")
                        prompt = "Conversation transcript from the UI (context only; do not repeat prior actions):\n" + json.dumps(prefix, ensure_ascii=False) + "\n\nCurrent user request:\n" + prompt
                    client.session_id = sid
                    await status("正在思考…")
                    completion = await client.begin("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": prompt}]})
                    async with asyncio.timeout(self.valves.TIMEOUT_SECONDS), cancel_before_disconnect(client, sid):
                        async for event in client.updates(completion):
                            method, params = event.get("method"), event.get("params", {})
                            if "id" in event:
                                if method == "session/request_permission":
                                    task = asyncio.create_task(self.permission(client, event, __event_call__, sid))
                                    dialog_id = params.get("_meta", {}).get("pi-rust/dialog", {}).get("requestId")
                                    requests[event["id"]] = (task, dialog_id)
                                else:
                                    await client.reply(event["id"], error={"code": -32601, "message": "Client method not supported"})
                                continue
                            if method == "_rpi/dialog_closed":
                                for request, dialog_id in requests.values():
                                    if dialog_id and dialog_id == params.get("requestId"):
                                        request.cancel()
                                continue
                            if method != "session/update" or params.get("sessionId") != sid:
                                continue
                            update = params.get("update", {})
                            kind = update.get("sessionUpdate")
                            if kind in ("agent_message_chunk", "agent_thought_chunk"):
                                content = update.get("content", {})
                                if content.get("type") != "text":
                                    continue
                                text = content.get("text", "")
                                if kind == "agent_thought_chunk":
                                    if not thinking:
                                        yield emit("<think>")
                                        thinking = True
                                    yield emit(text)
                                else:
                                    if thinking:
                                        yield emit("</think>\n")
                                        thinking = False
                                    yield emit(text)
                            elif kind in ("tool_call", "tool_call_update"):
                                if thinking:
                                    yield emit("</think>\n")
                                    thinking = False
                                tool_id = update.get("toolCallId")
                                tool = tools.setdefault(tool_id, {})
                                tool.update(update)
                                if tool.get("status") in ("completed", "failed"):
                                    if not tool.get("_shown"):
                                        yield emit(tool_detail(tool))
                                        tool["_shown"] = True
                                    await status("正在思考…")
                                else:
                                    await status("正在执行 · " + str(tool.get("title", "工具"))[:160])
                            elif kind == "plan":
                                await status("计划 · " + "；".join(str(e.get("content", "")) for e in update.get("entries", []))[:300])
                    result = await completion
                    if thinking:
                        yield emit("</think>\n")
                    reason = result.get("stopReason")
                    if reason == "cancelled":
                        await status("已停止", True)
                    elif reason in ("end_turn", "max_tokens", "max_turn_requests"):
                        completed = True
                        await status("已完成" if reason == "end_turn" else "已达到输出限制", True)
                    else:
                        raise AcpError(f"ACP stopped: {reason}")
        except asyncio.CancelledError:
            if client and sid:
                with contextlib.suppress(Exception):
                    await asyncio.wait_for(client.notify("session/cancel", {"sessionId": sid}), 2)
            await status("已停止", True)
            raise
        except Exception as exc:
            await status("执行失败", True)
            yield {"error": {"detail": str(exc)}}
        finally:
            for request, _ in requests.values():
                request.cancel()
            if requests:
                await asyncio.gather(*(r for r, _ in requests.values()), return_exceptions=True)
            if completion and not completion.done():
                completion.cancel()
            if client:
                await client.close()
            if completion and completion.done() and not completion.cancelled():
                completion.exception()
            fingerprint = digest(canonical(messages + [{"role": "assistant", "content": "".join(rendered)}])) if completed else None
            await asyncio.to_thread(store.finish, key, lease, sid if completed else None, fingerprint)
