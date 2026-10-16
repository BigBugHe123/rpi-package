"""Real WebSocket tests with a deterministic ACP peer; no model/API calls."""
import asyncio
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

from aiohttp import web, WSMsgType

source = Path(__file__).parents[1] / "openwebui/pipe.py"
spec = importlib.util.spec_from_file_location("ui_adapter_pipe", source)
adapter = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = adapter
spec.loader.exec_module(adapter)


class UiAdapterTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.calls = []
        self.sessions = set()
        self.cancelled = asyncio.Event()
        self.prompt_started = asyncio.Event()
        self.permission_answers = []
        self.events = []
        self.open_sockets = set()
        app = web.Application()
        app.router.add_get("/acp", self.peer)
        self.runner = web.AppRunner(app)
        await self.runner.setup()
        site = web.TCPSite(self.runner, "127.0.0.1", 0)
        await site.start()
        port = site._server.sockets[0].getsockname()[1]
        self.pipe = adapter.Pipe()
        self.pipe.valves.ADAPTER_URL = f"http://127.0.0.1:{port}"
        self.pipe.valves.ADAPTER_TOKEN = "test-adapter-key"
        self.pipe.valves.CWD = "/project"
        self.pipe.valves.STATE_DIR = self.temp.name

    async def asyncTearDown(self):
        for socket in list(self.open_sockets):
            await socket.close()
        await self.runner.cleanup()
        self.temp.cleanup()

    async def peer(self, request):
        if request.headers.get("Authorization") != "Bearer test-adapter-key":
            raise web.HTTPUnauthorized()
        ws = web.WebSocketResponse()
        await ws.prepare(request)
        self.open_sockets.add(ws)
        current = None

        async def update(sid, kind, **data):
            await ws.send_json({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": kind, **data}}})

        async def respond(message, result):
            await ws.send_json({"jsonrpc": "2.0", "id": message["id"], "result": result})

        try:
            async for frame in ws:
                if frame.type != WSMsgType.TEXT:
                    break
                message = json.loads(frame.data)
                method, params = message.get("method"), message.get("params", {})
                self.calls.append(message)
                if method == "initialize":
                    await respond(message, {"protocolVersion": 1, "agentCapabilities": {"loadSession": True, "sessionCapabilities": {"resume": {}}}})
                elif method == "session/new":
                    sid = "session-" + str(len(self.sessions))
                    self.sessions.add(sid)
                    await respond(message, {"sessionId": sid})
                elif method == "session/resume":
                    await update(params["sessionId"], "agent_message_chunk", content={"type": "text", "text": "HISTORY_MUST_NOT_REPLAY"})
                    await respond(message, {})
                elif method == "session/prompt":
                    current = message
                    sid = params["sessionId"]
                    prompt = params["prompt"][0]["text"]
                    self.prompt_started.set()
                    if prompt == "wait":
                        continue
                    if prompt == "error":
                        await ws.send_json({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32000, "message": "backend failed"}})
                        continue
                    if prompt == "disconnect":
                        await ws.close()
                        break
                    if prompt == "permission":
                        await ws.send_json({"jsonrpc": "2.0", "id": 99, "method": "session/request_permission", "params": {"sessionId": sid, "toolCall": {"title": "bash", "rawInput": {"command": "pwd"}}, "options": [{"optionId": "yes", "kind": "allow_once"}, {"optionId": "no", "kind": "reject_once"}]}})
                        continue
                    if prompt == "tools":
                        await update(sid, "agent_thought_chunk", content={"type": "text", "text": "checking"})
                        await update(sid, "tool_call", toolCallId="tool-1", title="bash <script>bad()</script>", status="in_progress", rawInput={"command": "pwd"})
                        await update(sid, "tool_call_update", toolCallId="tool-1", status="completed", rawOutput="</pre><script>bad()</script>")
                    await update(sid, "agent_message_chunk", content={"type": "text", "text": "回答🌊"})
                    # Completion deliberately arrives after the last update,
                    # with no later event to wake a broken queue-only reader.
                    await asyncio.sleep(0.01)
                    await respond(message, {"stopReason": "end_turn"})
                elif method == "session/cancel":
                    self.cancelled.set()
                    if current:
                        await respond(current, {"stopReason": "cancelled"})
                elif method is None and message.get("id") == 99:
                    self.permission_answers.append(message["result"])
                    await update(current["params"]["sessionId"], "agent_message_chunk", content={"type": "text", "text": "permission handled"})
                    await respond(current, {"stopReason": "end_turn"})
        finally:
            self.open_sockets.discard(ws)
        return ws

    async def emit(self, event):
        self.events.append(event)

    async def turn(self, text, history=None, user="one", chat="chat", event_call=None):
        messages = (history or []) + [{"role": "user", "content": text}]
        parts = [part async for part in self.pipe.pipe({"messages": messages}, __user__={"id": user}, __chat_id__=chat, __event_emitter__=self.emit, __event_call__=event_call)]
        return parts, messages

    async def test_completion_race_and_resume_without_replay(self):
        parts, messages = await asyncio.wait_for(self.turn("first"), 2)
        self.assertEqual(parts, ["回答🌊"])
        # Recreate the plugin instance to verify the durable session index.
        valves = self.pipe.valves
        self.pipe = adapter.Pipe()
        self.pipe.valves = valves
        second, _ = await self.turn("second", messages + [{"role": "assistant", "content": "".join(parts)}])
        self.assertEqual(second, ["回答🌊"])
        self.assertEqual(sum(c.get("method") == "session/new" for c in self.calls), 1)
        self.assertTrue(any(c.get("method") == "session/resume" for c in self.calls))
        prompts = [c["params"]["prompt"][0]["text"] for c in self.calls if c.get("method") == "session/prompt"]
        self.assertEqual(prompts, ["first", "second"])
        self.assertTrue(self.events[-1]["data"]["done"])

    async def test_branch_and_user_isolation(self):
        await self.turn("first")
        await self.turn("regenerated")
        await self.turn("other user", user="two")
        self.assertEqual(sum(c.get("method") == "session/new" for c in self.calls), 3)
        self.assertFalse(any(c.get("method") == "session/resume" for c in self.calls))

    async def test_tools_render_as_inert_details_and_survive_resume(self):
        parts, messages = await self.turn("tools")
        rendered = "".join(parts)
        self.assertIn('<details type="tool_calls" data-adapter="rpi-ui-adapter"', rendered)
        self.assertIn("&lt;script&gt;", rendered)
        self.assertNotIn("<script>", rendered)
        self.assertTrue(any("正在执行" in e["data"]["description"] for e in self.events))
        await self.turn("next", messages + [{"role": "assistant", "content": rendered}])
        self.assertTrue(any(c.get("method") == "session/resume" for c in self.calls))

    async def test_permission_bypass_and_disconnected_confirmation(self):
        self.pipe.valves.PERMISSIONS = "bypass"
        await self.turn("permission")
        self.assertEqual(self.permission_answers[-1], {"outcome": {"outcome": "selected", "optionId": "yes"}})
        self.pipe.valves.PERMISSIONS = "ask"

        async def disconnected(event):
            return {"error": "Client session disconnected."}

        await self.turn("permission", chat="another", event_call=disconnected)
        self.assertEqual(self.permission_answers[-1], {"outcome": {"outcome": "cancelled"}})

    async def test_stop_sends_cancel_and_clears_busy_lease(self):
        task = asyncio.create_task(self.turn("wait"))
        await asyncio.wait_for(self.prompt_started.wait(), 2)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        await asyncio.wait_for(self.cancelled.wait(), 2)
        parts, _ = await self.turn("retry")
        self.assertEqual(parts, ["回答🌊"])

    async def test_failed_and_disconnected_runs_do_not_hang_or_resume(self):
        for text in ("error", "disconnect"):
            parts, _ = await asyncio.wait_for(self.turn(text), 2)
            self.assertIn("error", parts[-1])
        await self.turn("after")
        self.assertFalse(any(c.get("method") == "session/resume" for c in self.calls))

    async def test_parallel_turn_is_rejected_and_background_tasks_are_skipped(self):
        task = asyncio.create_task(self.turn("wait"))
        await asyncio.wait_for(self.prompt_started.wait(), 2)
        try:
            with self.assertRaisesRegex(ValueError, "already has a running"):
                await self.turn("overlap")
            parts = [p async for p in self.pipe.pipe({}, __task__="title_generation")]
            self.assertEqual(parts, [""])
        finally:
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task

    async def test_authentication_failure_is_reported(self):
        self.pipe.valves.ADAPTER_TOKEN = "wrong"
        parts, _ = await asyncio.wait_for(self.turn("test"), 2)
        self.assertIn("error", parts[-1])


if __name__ == "__main__":
    unittest.main()
