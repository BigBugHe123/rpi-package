"""Optional real-host checks: ACP stdio lifecycle and Zed registration."""
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import unittest


@unittest.skipUnless(os.environ.get("RPI_TEST_HOST") and os.environ.get("RPI_TEST_PLUGIN"),
                     "set RPI_TEST_HOST and RPI_TEST_PLUGIN for native plugin checks")
class AcpPluginTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="rpi-acp-plugin-")
        self.root = Path(self.temp.name)
        extensions = self.root / "agent/extensions"
        extensions.mkdir(parents=True)
        plugin = Path(os.environ["RPI_TEST_PLUGIN"])
        shutil.copy2(plugin, extensions / plugin.name)
        self.workspace = self.root / "project with spaces"
        self.workspace.mkdir()
        self.env = os.environ.copy()
        self.env.update(
            RPI_CODING_AGENT_DIR=str(self.root / "agent"),
            RPI_EXTENSIONS_DIR=str(extensions),
            USERPROFILE=str(self.root), HOME=str(self.root),
            ANTHROPIC_API_KEY="offline-plugin-fixture",
            RPI_ACP_NO_AUTO_REGISTER_ZED="1",
            RPI_ACP_SESSION_DIR=str(self.root / "sessions"),
        )
        if os.environ.get("RPI_TEST_RPC_PEER"):
            self.env["RPI_BIN"] = os.environ["RPI_TEST_RPC_PEER"]
        self.processes = []
        self.readers = []

    def tearDown(self):
        for process in self.processes:
            if process.poll() is None:
                process.stdin.close()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            if not process.stdin.closed:
                process.stdin.close()
        for reader in self.readers:
            reader.join(timeout=5)
        for process in self.processes:
            process.stdout.close()
            process.stderr.close()
        self.temp.cleanup()

    def start(self, *args):
        process = subprocess.Popen([
            os.environ["RPI_TEST_HOST"], "--offline", "--no-session", "--local-only",
            "--provider", "anthropic", "--model", "claude-sonnet-4-5", *args,
        ], cwd=self.workspace, env=self.env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
        self.processes.append(process)
        output = queue.Queue()
        errors = []
        for reader in (
            threading.Thread(target=lambda: [output.put(line) for line in process.stdout], daemon=True),
            threading.Thread(target=lambda: [errors.append(line) for line in process.stderr], daemon=True),
        ):
            reader.start()
            self.readers.append(reader)
        return process, output, errors

    def response(self, output, request_id):
        while True:
            message = json.loads(output.get(timeout=15))
            if message.get("id") == request_id:
                return message

    def test_stdio_mode_serves_acp_and_finishes_on_eof(self):
        process, output, errors = self.start("--acp")
        process.stdin.write(json.dumps({"jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":1, "clientCapabilities":{"_meta":{"pi-rust/questions":1}}}}) + "\n")
        process.stdin.flush()
        self.assertEqual(self.response(output, 1)["result"]["protocolVersion"], 1)
        if os.environ.get("RPI_TEST_RPC_PEER"):
            process.stdin.write(json.dumps({"jsonrpc":"2.0", "id":2, "method":"session/new",
                "params":{"cwd":str(self.workspace),"mcpServers":[]}}) + "\n")
            process.stdin.flush()
            self.assertTrue(self.response(output, 2)["result"]["sessionId"])
        process.stdin.close()
        self.assertEqual(process.wait(timeout=15), 0, "".join(errors))

    def test_print_zed_exits_while_stdin_remains_open(self):
        process, output, errors = self.start("--print-zed")
        self.assertEqual(process.wait(timeout=15), 0, "".join(errors))
        for reader in self.readers:
            reader.join(timeout=5)
        snippet = json.loads("".join(list(output.queue)))
        self.assertEqual(snippet["agent_servers"]["rpi"]["args"], ["--acp"])

    def test_install_zed_registers_then_exits_without_waiting_for_stdin(self):
        settings = self.root / "zed-settings.json"
        self.env["ZED_SETTINGS"] = str(settings)
        process, _, errors = self.start("--install-zed")
        self.assertEqual(process.wait(timeout=15), 0, "".join(errors))
        text = settings.read_text(encoding="utf-8")
        registration = json.loads("\n".join(line for line in text.splitlines()
                                           if not line.lstrip().startswith("//")))
        self.assertEqual(registration["agent_servers"]["rpi"]["args"], ["--acp"])
