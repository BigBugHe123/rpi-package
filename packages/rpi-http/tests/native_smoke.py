"""Verify the headless HTTP service with a real rpi host; never call a model."""
import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--http', required=True)
parser.add_argument('--rpi', required=True)
args = parser.parse_args()
binary, host = str(Path(args.http).resolve()), str(Path(args.rpi).resolve())

with tempfile.TemporaryDirectory(prefix='rpi-http-native-') as directory:
    root = Path(directory)
    agent = root / 'agent'
    (agent / 'extensions').mkdir(parents=True)
    (agent / 'settings.json').write_text(json.dumps({'defaultProvider': 'anthropic', 'defaultModel': 'claude-sonnet-4-5'}))
    env = dict(os.environ, RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / 'extensions'),
               RPI_OFFLINE='1', ANTHROPIC_API_KEY='fixture-only', RPI_HTTP_PASSWORD='fixture-password')
    for name in ['RPI_HTTP_ALLOWED_HOSTS', 'RPI_WEB_ALLOWED_HOSTS', 'RPI_WEB_PASSWORD']:
        env.pop(name, None)
    config = root / 'config' / 'http.json'
    config.parent.mkdir()
    (root / 'workspace').mkdir()
    config.write_text(json.dumps({'project': '../workspace', 'rpiBin': host, 'port': 31337,
                                 'registry': 'state/sessions.json', 'sessionDir': 'history'}))
    process = None
    log = None

    def request(path, data=None, method=None):
        payload = None if data is None else json.dumps(data).encode()
        req = urllib.request.Request(base + path, payload, {'Content-Type': 'application/json', 'X-Rpi-Web': '1'}, method=method)
        with opener.open(req, timeout=15) as response:
            if response.status == 204:
                return None
            return json.load(response)

    def expect_status(path, status, body=None):
        req = urllib.request.Request(base + path, body, {'Content-Type': 'application/json', 'X-Rpi-Web': '1'})
        try:
            with opener.open(req, timeout=15) as response:
                assert response.status == status
        except urllib.error.HTTPError as error:
            assert error.code == status, (path, error.code, error.read())

    def start():
        global process, log, base, opener
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        log = (root / 'server.log').open('w', encoding='utf-8')
        process = subprocess.Popen([binary, '--port', '0', '--config', str(config.relative_to(root))], cwd=root, env=env,
            stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        for _ in range(150):
            text = (root / 'server.log').read_text(encoding='utf-8')
            assert process.poll() is None, text
            match = re.search(r'listening on (127\.0\.0\.1:\d+)', text)
            if match:
                base = 'http://' + match[1]
                break
            time.sleep(.1)
        else:
            raise AssertionError('HTTP startup timed out: ' + text)
        assert request('/api/v1/health')['service'] == 'rpi-http'
        expect_status('/api/v1/sessions', 401)
        assert request('/api/v1/auth', {'password': 'fixture-password'})['authenticated']

    def stop():
        global process, log
        if process is not None and process.poll() is None:
            if os.name == 'nt':
                for session in request('/api/v1/sessions')['sessions']:
                    if session['status'] != 'offline':
                        try:
                            request('/api/v1/sessions/' + session['sessionId'] + '/commands', {'type': 'stop'})
                        except urllib.error.URLError:
                            pass
                process.terminate()
            else:
                process.send_signal(signal.SIGINT)
            process.wait(timeout=15)
        process = None
        if log is not None:
            log.close()
            log = None

    try:
        start()
        expect_status('/', 404)
        expect_status('/assets/index.js', 404)
        expect_status('/api/v1/sessions', 400, b'{')
        assert 'prompt' in request('/api/v1/capabilities')['supportedCommands']
        sid = request('/api/v1/sessions', {})['sessionId']
        path = '/api/v1/sessions/' + sid
        assert request(path + '/commands', {'type': 'get_state'})['success']
        assert list((config.parent / 'history').glob('**/*.jsonl')), 'session history must resolve against the config directory'
        assert not (root / 'workspace' / 'config').exists(), 'paths must not be reinterpreted relative to the child cwd'
        request(path + '/rename', {'title': 'Shared HTTP session'})
        request(path + '/metadata', {'tags': ['http', 'shared'], 'pinned': True})
        assert request(path + '/snapshot')['sessionId'] == sid
        with opener.open(base + path + '/events', timeout=10) as stream:
            assert stream.readline().startswith(b'event: ready')
        transient = request('/api/v1/sessions', {})['sessionId']
        transient_path = '/api/v1/sessions/' + transient
        with opener.open(base + transient_path + '/events', timeout=10) as stream:
            assert stream.readline().startswith(b'event: ready')
            request(transient_path, method='DELETE')
            for _ in range(64):
                line = stream.readline()
                if line.startswith(b'event: host_closed'):
                    break
                assert line, 'SSE ended before delivering host_closed'
            else:
                raise AssertionError('SSE did not publish host_closed')
        stop()
        start()
        session = next(session for session in request('/api/v1/sessions')['sessions'] if session['sessionId'] == sid)
        assert session['title'] == 'Shared HTTP session' and session['pinned']
        assert session['tags'] == ['http', 'shared']
        request(path + '/open', {})
        assert request(path + '/commands', {'type': 'get_state'})['success']
        print(json.dumps({'passed': True, 'headless': True, 'nativeRpc': True, 'snapshot': True,
            'sse': True, 'sessionCloseEvent': True, 'relativeConfigPaths': True,
            'authentication': True, 'restart': True, 'metadata': True, 'noModelCalls': True}))
    finally:
        stop()
