#!/usr/bin/env python3
"""
AeroCloud Nemo Extension
Provides sync status overlay emblems for files tracked by AeroCloud.
Communicates with AeroCloud badge daemon via Unix socket (Nextcloud-compatible protocol).
"""

import os
import socket
import time
from pathlib import Path
from threading import Lock
from typing import Optional, Dict

import gi

gi.require_version('Nemo', '3.0')
from gi.repository import Nemo, GObject


class AeroCloudStatusCache:
    """Thread-safe cache for file status results with TTL."""

    def __init__(self, ttl_seconds: int = 5):
        self._cache: Dict[str, tuple] = {}
        self._lock = Lock()
        self._ttl = ttl_seconds

    def get(self, path: str) -> Optional[str]:
        with self._lock:
            if path in self._cache:
                status, timestamp = self._cache[path]
                if time.time() - timestamp < self._ttl:
                    return status
                else:
                    del self._cache[path]
        return None

    def set(self, path: str, status: str) -> None:
        with self._lock:
            self._cache[path] = (status, time.time())


class AeroCloudDaemonClient:
    """Client for communicating with AeroCloud badge daemon via Unix socket."""

    SOCKET_TIMEOUT = 0.1  # 100ms
    RECONNECT_INTERVAL = 30  # seconds

    def __init__(self):
        self._socket: Optional[socket.socket] = None
        self._socket_path: Optional[str] = None
        self._last_connect_attempt = 0.0
        self._lock = Lock()
        self._find_socket_path()

    def _find_socket_path(self) -> None:
        runtime_dir = os.environ.get('XDG_RUNTIME_DIR')
        if runtime_dir:
            self._socket_path = os.path.join(runtime_dir, 'aerocloud', 'socket')
        else:
            uid = os.getuid()
            self._socket_path = f'/tmp/aerocloud-{uid}/socket'

    def _connect(self) -> bool:
        now = time.time()
        if now - self._last_connect_attempt < self.RECONNECT_INTERVAL:
            return self._socket is not None

        self._last_connect_attempt = now

        if self._socket:
            try:
                self._socket.close()
            except (socket.error, OSError):
                pass
            self._socket = None

        if not self._socket_path or not os.path.exists(self._socket_path):
            return False

        try:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(self.SOCKET_TIMEOUT)
            sock.connect(self._socket_path)
            self._socket = sock
            return True
        except (socket.error, OSError):
            return False

    def query_status(self, file_path: str) -> Optional[str]:
        with self._lock:
            if not self._socket and not self._connect():
                return None

            try:
                request = f"RETRIEVE_FILE_STATUS\npath\t{file_path}\ndone\n"
                self._socket.sendall(request.encode('utf-8'))

                response = self._socket.recv(1024).decode('utf-8', errors='ignore')

                for line in response.splitlines():
                    if line.startswith('STATUS:'):
                        parts = line.split(':', 2)
                        if len(parts) >= 3:
                            return parts[1]

                return None

            except (socket.timeout, socket.error, OSError, BrokenPipeError):
                if self._socket:
                    try:
                        self._socket.close()
                    except (socket.error, OSError):
                        pass
                    self._socket = None
                return None


class AeroCloudExtension(GObject.GObject, Nemo.InfoProvider):
    """Nemo extension that adds AeroCloud sync status emblems."""

    # Custom AeroCloud emblems (installed as SVGs in hicolor icon theme)
    STATUS_EMBLEMS = {
        'OK': 'emblem-aerocloud-synced',
        'SYNC': 'emblem-aerocloud-syncing',
        'ERROR': 'emblem-aerocloud-error',
        'IGNORE': 'emblem-aerocloud-ignored',
        'CONFLICT': 'emblem-aerocloud-conflict',
        'NEW': 'emblem-aerocloud-new',
        'NOP': None,
    }

    def __init__(self):
        super().__init__()
        self._cache = AeroCloudStatusCache(ttl_seconds=5)
        self._client = AeroCloudDaemonClient()

    def update_file_info(self, file: Nemo.FileInfo) -> Nemo.OperationResult:
        if file.get_uri_scheme() != 'file':
            return Nemo.OperationResult.COMPLETE

        file_path = file.get_location().get_path()
        if not file_path:
            return Nemo.OperationResult.COMPLETE

        cached_status = self._cache.get(file_path)
        if cached_status is not None:
            self._apply_emblem(file, cached_status)
            return Nemo.OperationResult.COMPLETE

        status = self._client.query_status(file_path)

        if status:
            self._cache.set(file_path, status)
            self._apply_emblem(file, status)

        return Nemo.OperationResult.COMPLETE

    def _apply_emblem(self, file: Nemo.FileInfo, status: str) -> None:
        emblem = self.STATUS_EMBLEMS.get(status)
        if emblem:
            file.add_emblem(emblem)
