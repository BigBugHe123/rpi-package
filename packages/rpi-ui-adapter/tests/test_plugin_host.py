"""Native CLI integration, automatic installation and ACP relay checks.

Set RPI_TEST_HOST, RPI_TEST_UI_PLUGIN and RPI_TEST_ACP_PLUGIN to enable.
The mock Open WebUI API and ACP initialize make no model calls.
"""
import asyncio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

import aiohttp


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


@unittest.skipUnless(all(os.environ.get(name) for name in ("RPI_TEST_HOST", "RPI_TEST_UI_PLUGIN", "RPI_TEST_ACP_PLUGIN")), "set native rpi and plugin paths")
class NativeUiAdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.extensions = self.root / "agent/extensions"
        self.extensions.mkdir(parents=True)
        for name in ("RPI_TEST_UI_PLUGIN", "RPI_TEST_ACP_PLUGIN"):
            source = Path(os.environ[name])
            shutil.copy2(source, self.extensions / source.name)
        self.records = {}
        records = self.records

        class Api(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def request(self):
                if self.headers.get("Authorization") != "Bearer fixture-admin":
                    self.send_error(401)
                    return
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or "null")
                path = self.path
                if self.command == "POST":
                    if path.endswith(("/create", "/update")) and "/valves/" not in path:
                        records["function"] = {**body, "is_active": records.get("function", {}).get("is_active", False)}
                    elif path.endswith("/valves/update"):
                        records["valves"] = body
                    elif path.endswith("/toggle"):
                        records["function"]["is_active"] = not records["function"]["is_active"]
                if path == "/api/v1/functions/":
                    result = [records["function"]] if "function" in records else []
                elif path.endswith("/valves") or path.endswith("/valves/update"):
                    result = records.get("valves", {})
                elif "function" in records:
                    result = records["function"]
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps(result).encode())

            do_GET = do_POST = request

        self.api = ThreadingHTTPServer(("127.0.0.1", 0), Api)
        self.api_thread = threading.Thread(target=self.api.serve_forever, daemon=True)
        self.api_thread.start()
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.env = os.environ.copy()
        self.env.update(RPI_CODING_AGENT_DIR=str(self.root / "agent"), RPI_EXTENSIONS_DIR=str(self.extensions),
                        RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_OFFLINE="1", ANTHROPIC_API_KEY="fixture-only")
        self.env.pop("OPENWEBUI_TOKEN", None)
        self.processes = []
        self.readers = []

    def tearDown(self):
        for process in self.processes:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=15)
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()
        for reader in self.readers:
            reader.join(timeout=5)
        self.api.shutdown()
        self.api.server_close()
        self.api_thread.join(timeout=5)
        self.temp.cleanup()

    def start(self, *args):
        process = subprocess.Popen([os.environ["RPI_TEST_HOST"], "--offline", "--no-session", "--local-only",
            "--provider", "anthropic", "--model", "claude-sonnet-4-5", "--extensions-dir", str(self.extensions), *args],
            cwd=self.root, env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8")
        self.processes.append(process)
        errors = []
        for stream in (process.stdout, process.stderr):
            reader = threading.Thread(target=lambda s=stream: errors.extend(s.readlines()), daemon=True)
            reader.start()
            self.readers.append(reader)
        return process, errors

    def ready(self, process, errors):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        for _ in range(100):
            if process.poll() is not None:
                self.fail("rpi exited: " + "".join(errors))
            try:
                with opener.open(self.base + "/health", timeout=.2) as response:
                    self.assertEqual(response.read(), b"rpi-ui-adapter")
                path = self.root / "agent/ui-adapter/openwebui.json"
                if path.exists() and json.loads(path.read_text()).get("installed"):
                    return json.loads(path.read_text())
            except (OSError, json.JSONDecodeError):
                pass
            time.sleep(.1)
        self.fail("adapter did not start: " + "".join(errors))

    def test_auto_install_native_acp_and_saved_restart(self):
        process, errors = self.start("--webui=openwebui", "--webui-url", f"http://127.0.0.1:{self.api.server_port}",
            "--webui-bind", f"127.0.0.1:{self.port}", "--webui-token", "fixture-admin", "--webui-permissions", "bypass")
        config = self.ready(process, errors)
        self.assertTrue(self.records["function"]["is_active"])
        self.assertEqual(self.records["valves"]["PERMISSIONS"], "bypass")
        self.assertNotIn("fixture-admin", json.dumps(config))

        async def probe():
            async with aiohttp.ClientSession(trust_env=False) as client:
                with self.assertRaises(aiohttp.WSServerHandshakeError) as rejected:
                    await client.ws_connect(self.base + "/acp", headers={"Authorization": "Bearer wrong"})
                self.assertEqual(rejected.exception.status, 401)
                async with client.ws_connect(self.base + "/acp", headers={"Authorization": "Bearer " + config["adapter_token"]}) as ws:
                    await ws.send_json({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":1,"clientCapabilities":{}}})
                    response = await asyncio.wait_for(ws.receive_json(), 15)
                    self.assertEqual(response["result"]["protocolVersion"], 1)
                    self.assertEqual(response["result"]["agentInfo"]["name"], "rpi")

        asyncio.run(probe())
        process.terminate()
        process.wait(timeout=15)
        restarted, errors = self.start("--webui=openwebui")
        self.ready(restarted, errors)

    def test_unknown_ui_fails_with_actionable_error(self):
        process, errors = self.start("--webui=unknown")
        self.assertEqual(process.wait(timeout=15), 2)
        for reader in self.readers:
            reader.join(timeout=5)
        self.assertIn("Unsupported UI", "".join(errors))


if __name__ == "__main__":
    unittest.main()
