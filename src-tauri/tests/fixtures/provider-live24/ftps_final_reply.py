#!/usr/bin/env python3
"""Real TLS FTP post-data final-reply acceptance (loopback only)."""
import argparse
import hashlib
import json
import os
import pathlib
import socket
import socketserver
import ssl
import struct
import subprocess
import tempfile
import threading
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--cli', default='aeroftp-cli')
parser.add_argument('--output', type=pathlib.Path, help='New private evidence directory; must not exist')
options = parser.parse_args()
ROOT = options.output or pathlib.Path(tempfile.mkdtemp(prefix='aeroftp-ftps-final-reply-'))
if options.output:
    ROOT.mkdir(mode=0o700, parents=True)

def openssl(*args):
    subprocess.run(['openssl', *args], cwd=ROOT, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
if not (ROOT / 'server.pem').exists():
    openssl('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '2', '-keyout', 'ca.key', '-out', 'ca.pem', '-subj', '/CN=AeroFTP live24 fixture CA', '-addext', 'basicConstraints=critical,CA:TRUE')
    openssl('req', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'server.key', '-out', 'server.csr', '-subj', '/CN=localhost')
    (ROOT / 'extensions.cnf').write_text('subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n')
    openssl('x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key', '-CAcreateserial', '-days', '2', '-out', 'server.pem', '-extfile', 'extensions.cnf')
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_2
context.load_cert_chain(ROOT / 'server.pem', ROOT / 'server.key')
payload = b'AeroFTP live24 payload\n' * 4096
path = ROOT / 'payload.bin'
path.write_bytes(payload)

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

class Handler(socketserver.BaseRequestHandler):

    def send(self, line):
        self.conn.sendall((line + '\r\n').encode())

    def line(self):
        data = bytearray()
        while not data.endswith(b'\n'):
            byte = self.conn.recv(1)
            if not byte:
                return None
            data.extend(byte)
            if len(data) > 4096:
                raise RuntimeError('Overlong command')
        return data.decode().strip()

    def handle(self):
        self.conn = self.request
        self.conn.settimeout(15)
        data_listener = None
        try:
            if self.server.mode == 'implicit':
                self.conn = context.wrap_socket(self.conn, server_side=True)
            self.send('220 AeroFTP live24 FTPS fixture')
            while True:
                line = self.line()
                if line is None:
                    break
                verb, _, arg = line.partition(' ')
                verb = verb.upper()
                self.server.events.append({'command': verb})
                if verb == 'AUTH':
                    self.send('234 Ready for TLS')
                    self.conn = context.wrap_socket(self.conn, server_side=True)
                elif verb == 'USER':
                    self.send('331 Password required')
                elif verb == 'PASS':
                    self.send('230 Logged in')
                elif verb == 'FEAT':
                    self.send('211-Features')
                    self.send(' MLST type*;size*;modify*;')
                    self.send(' UTF8')
                    self.send('211 End')
                elif verb == 'PWD':
                    self.send('257 "/" is current')
                elif verb in ('TYPE', 'PBSZ', 'PROT', 'OPTS', 'NOOP', 'CWD'):
                    self.send('200 OK')
                elif verb in ('SIZE', 'MDTM', 'MLST'):
                    self.send('550 File not found')
                elif verb in ('PASV', 'EPSV'):
                    if data_listener:
                        data_listener.close()
                    data_listener = socket.socket()
                    data_listener.bind(('127.0.0.1', 0))
                    data_listener.listen(1)
                    data_listener.settimeout(15)
                    port = data_listener.getsockname()[1]
                    self.send(f'227 Entering Passive Mode (127,0,0,1,{port // 256},{port % 256})' if verb == 'PASV' else f'229 Entering Extended Passive Mode (|||{port}|)')
                elif verb in ('LIST', 'MLSD'):
                    self.send('150 Listing')
                    raw, _ = data_listener.accept()
                    data = context.wrap_socket(raw, server_side=True)
                    data.close()
                    data_listener.close()
                    data_listener = None
                    self.send('226 Listing complete')
                elif verb == 'STOR':
                    self.send('150 Send file')
                    raw, _ = data_listener.accept()
                    data = context.wrap_socket(raw, server_side=True)
                    received = bytearray()
                    while len(received) < len(payload):
                        chunk = data.recv(min(65536, len(payload) - len(received)))
                        if not chunk:
                            break
                        received.extend(chunk)
                    self.server.events.append({'received_bytes': len(received), 'sha256': hashlib.sha256(received).hexdigest(), 'data_tls': data.version(), 'control_tls': self.conn.version(), 'refusal': self.server.refuse})
                    if self.server.reset:
                        fd = data.detach()
                        raw = socket.socket(fileno=fd)
                        raw.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack('ii', 1, 0))
                        raw.close()
                    else:
                        data.close()
                    data_listener.close()
                    data_listener = None
                    self.send('552 Quota exceeded after receiving data' if self.server.refuse else '226 Transfer complete')
                elif verb == 'QUIT':
                    self.send('221 Bye')
                    break
                else:
                    self.send('200 OK')
        except (OSError, ssl.SSLError) as error:
            self.server.events.append({'server_error': type(error).__name__})
        finally:
            if data_listener:
                data_listener.close()
            self.conn.close()
results = []
for mode in ['explicit', 'implicit']:
    for refuse, reset in [(False, False), (True, False), (True, True)]:
        server = Server(('127.0.0.1', 0), Handler)
        server.mode = mode
        server.refuse = refuse
        server.reset = reset
        server.events = []
        threading.Thread(target=server.serve_forever, daemon=True).start()
        env = dict(os.environ, SSL_CERT_FILE=str(ROOT / 'ca.pem'), SSL_CERT_DIR=str(ROOT / 'empty-ca-dir'))
        (ROOT / 'empty-ca-dir').mkdir(exist_ok=True)
        command = [options.cli, 'put', f'ftps://fixture@localhost:{server.server_address[1]}', str(path), '/payload.bin', '--tls', mode, '--password-stdin', '--json', '--no-banner']
        try:
            result = subprocess.run(command, input='artificial-fixture-password\n', capture_output=True, text=True, env=env, timeout=45)
            item = {'mode': mode, 'refused': refuse, 'reset': reset, 'exit': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr, 'events': server.events}
            results.append(item)
            (ROOT / 'receipt.json').write_text(json.dumps(results, indent=2))
            stor = sum((e.get('command') == 'STOR' for e in server.events))
            received = [e for e in server.events if 'received_bytes' in e]
            assert stor == 1, (mode, refuse, reset, stor, result.stderr)
            assert len(received) == 1 and received[0]['received_bytes'] == len(payload) and (received[0]['sha256'] == hashlib.sha256(payload).hexdigest())
            assert received[0]['data_tls'] == 'TLSv1.2' and received[0]['control_tls'] == 'TLSv1.2'
            assert result.returncode == (4 if refuse else 0), (mode, refuse, reset, result.returncode, result.stderr)
            if refuse:
                assert '552' in result.stdout + result.stderr
            print(json.dumps({'mode': mode, 'refused': refuse, 'reset': reset, 'STOR': stor, 'received': len(payload), 'exit': result.returncode, 'status': 'PASS'}), flush=True)
        finally:
            server.shutdown()
            server.server_close()
print(f"Evidence: {ROOT / 'receipt.json'}")
