#!/usr/bin/env python3
"""Exercise headless auth through the built CLI with a disposable portable vault."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import selectors
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from urllib.parse import parse_qs, urlparse

source = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='profile-auth-', dir=source.parent) as directory:
    root = Path(directory)
    cli = root / source.name
    try:
        os.link(source, cli)
    except OSError:
        shutil.copy2(source, cli)
    (root / 'portable.marker').touch()
    env = dict(os.environ, AEROFTP_MASTER_PASSWORD='synthetic headless auth master',
               XDG_CONFIG_HOME=str(root / 'config'), XDG_DATA_HOME=str(root / 'data'),
               XDG_CACHE_HOME=str(root / 'cache'), DBUS_SESSION_BUS_ADDRESS='unix:path=/nonexistent-profile-auth')

    def invoke(*args, success=True):
        result = subprocess.run([str(cli), '--machine', '--no-banner', *args], env=env,
                                cwd=root, capture_output=True, text=True, timeout=40)
        assert (result.returncode == 0) == success, (args, result.returncode, result.stderr)
        return json.loads(result.stdout) if result.returncode == 0 else result

    def auth(profile, request, success=True):
        path = root / 'request.json'
        path.write_text(json.dumps(request))
        return invoke('profile-auth', profile, '--request-json-file', str(path), success=success)

    if '--mcp-only' not in sys.argv and '--credentials-only' not in sys.argv:
        invoke('profile-add', '--name', 'headless-drive', '--protocol', 'googledrive')
        configured = auth('headless-drive', {'action': 'configure', 'client_id': 'synthetic-client', 'client_secret': 'synthetic-secret'})
        assert configured['status'] == 'configured'
        started = auth('headless-drive', {'action': 'begin', 'redirect_port': 19899})
        query = parse_qs(urlparse(started['authorization_url']).query)
        assert query['client_id'] == ['synthetic-client']
        assert query['redirect_uri'] == ['http://127.0.0.1:19899/callback']
        assert query['code_challenge_method'] == ['S256']
        assert query['state'][0] and query['code_challenge'][0]
        assert 'synthetic-secret' not in json.dumps(started)
        # A separate CLI process must reject a mismatching callback without exchanging a code.
        auth('headless-drive', {'action': 'finish', 'code': 'synthetic-code', 'state': 'wrong-state'}, success=False)
        auth('headless-drive', {'action': 'cancel'})
        assert auth('headless-drive', {'action': 'status'})['pending'] is False
        auth('headless-drive', {'action': 'finish', 'code': 'synthetic-code', 'state': query['state'][0]}, success=False)
        for protocol in ['dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive', 'yandexdisk', 'googlephotos']:
            name = 'headless-' + protocol
            if protocol == 'googlephotos' and '--dev' not in sys.argv:
                invoke('profile-add', '--name', name, '--protocol', protocol, success=False)
                continue
            invoke('profile-add', '--name', name, '--protocol', protocol)
            auth(name, {'action': 'configure', 'client_id': 'sibling-client', 'client_secret': 'sibling-secret'})
            started = auth(name, {'action': 'begin'})
            assert urlparse(started['authorization_url']).scheme == 'https', protocol
            assert 'sibling-secret' not in json.dumps(started), protocol
            auth(name, {'action': 'cancel'})
        print('Headless OAuth configuration, persisted PKCE, callback binding, cancellation, and public redaction pass.')

    if '--credentials-only' not in sys.argv:
        if '--mcp-only' in sys.argv:
            invoke('profile-add', '--name', 'headless-drive', '--protocol', 'googledrive')
        process = subprocess.Popen([str(cli), '--no-banner', 'mcp'], env=env, cwd=root,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        def rpc(number, method, params):
            process.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': number, 'method': method, 'params': params}) + '\n')
            process.stdin.flush()
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    assert selector.select(35), f'MCP timed out on {method}'
                    line = process.stdout.readline()
                    assert line, f'MCP exited on {method}'
                    response = json.loads(line)
                    if response.get('id') == number:
                        assert 'error' not in response, response
                        return response['result']
        try:
            rpc(1, 'initialize', {'protocolVersion': '2024-11-05', 'capabilities': {}, 'clientInfo': {'name': 'synthetic-auth-parity', 'version': '1'}})
            tools = rpc(2, 'tools/list', {})
            print(f"MCP exposes {len(tools['tools'])} tools.", flush=True)
            assert any(tool['name'] == 'aeroftp_profile_auth' for tool in tools['tools']), 'MCP has no headless authorization route'
            configured = rpc(3, 'tools/call', {'name': 'aeroftp_profile_auth', 'arguments': {'server': 'headless-drive', 'request': {'action': 'configure', 'client_id': 'mcp-client', 'client_secret': 'mcp-secret'}}})
            assert not configured.get('isError'), configured
            begun = rpc(4, 'tools/call', {'name': 'aeroftp_profile_auth', 'arguments': {'server': 'headless-drive', 'request': {'action': 'begin'}}})
            assert not begun.get('isError'), begun
            public = json.loads(begun['content'][0]['text'])
            assert parse_qs(urlparse(public['authorization_url']).query)['client_id'] == ['mcp-client']
            assert 'mcp-secret' not in json.dumps(begun)
            assert auth('headless-drive', {'action': 'status'})['pending'] is True
            auth('headless-drive', {'action': 'cancel'})
        finally:
            process.terminate()
            try:
                process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
        print('MCP and CLI share authorization configuration and pending state without exposing client secrets.')

    if '--mcp-only' in sys.argv:
        sys.exit(0)

    captured = []
    class Handler(BaseHTTPRequestHandler):
        def do_PROPFIND(self):
            captured.append(self.headers.get('Authorization'))
            self.send_response(401)
            self.send_header('WWW-Authenticate', 'Basic realm="synthetic"')
            self.end_headers()
        do_GET = do_PROPFIND
        def log_message(self, *_):
            pass
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    for key in ['password', 'api_key', 'access_token']:
        secret = 'synthetic-' + key
        invoke('profile-add', '--name', key, '--protocol', 'webdav',
               '--host', f'http://127.0.0.1:{server.server_port}', '--port', str(server.server_port),
               '--credential-json', json.dumps({'username': 'synthetic', key: secret}))
        invoke('--profile', key, 'ls', '/', success=False)
        expected = 'Basic ' + base64.b64encode(f'synthetic:{secret}'.encode()).decode()
        assert expected in captured, f'{key} was seeded but did not reach the real WebDAV request'
    server.shutdown()
    invoke('profile-add', '--name', 'invalid-wrapper', '--protocol', 'webdav',
           '--credential-json', '{"unknown_secret":"synthetic"}', success=False)
    print('All credential wrappers reached their actual HTTP authentication sink; unsupported seeding failed.')
