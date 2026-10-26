"""A small WebSocket client for the web app's e2e and benchmarks
(standard library only)."""

import base64
import http.client
import json
import os
import socket
import struct
import time


class WS:
    def __init__(self, host, port, cookie, origin=None, path="/ws", timeout=10):
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        key = base64.b64encode(os.urandom(16)).decode()
        origin = origin or f"http://{host}:{port}"
        head = (
            f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
            f"Origin: {origin}\r\nCookie: {cookie}\r\n\r\n"
        )
        self.sock.sendall(head.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            c = self.sock.recv(1)
            if not c:
                raise ConnectionError("closed during handshake")
            buf += c
        self.status = int(buf.split(b" ")[1])
        if self.status != 101:
            raise ConnectionError(f"handshake: {buf.decode(errors='replace')}")
        self.rbuf = b""
        self.last_state = None

    def _recv(self, n):
        while len(self.rbuf) < n:
            c = self.sock.recv(1 << 20)
            if not c:
                raise ConnectionError("closed")
            self.rbuf += c
        out, self.rbuf = self.rbuf[:n], self.rbuf[n:]
        return out

    def send(self, opcode, payload):
        mask = os.urandom(4)
        n = len(payload)
        if n < 126:
            h = struct.pack("!BB", 0x80 | opcode, 0x80 | n)
        elif n < 65536:
            h = struct.pack("!BBH", 0x80 | opcode, 0x80 | 126, n)
        else:
            h = struct.pack("!BBQ", 0x80 | opcode, 0x80 | 127, n)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(h + mask + masked)

    def send_json(self, v):
        self.send(1, json.dumps(v).encode())

    def send_input(self, uid, data):
        self.send(2, bytes([1]) + struct.pack("!I", uid) + data)

    def recv(self):
        """(opcode, payload) of the next whole message (answers pings)."""
        data = b""
        while True:
            b0, b1 = self._recv(2)
            op = b0 & 0x0F
            n = b1 & 0x7F
            if n == 126:
                n = struct.unpack("!H", self._recv(2))[0]
            elif n == 127:
                n = struct.unpack("!Q", self._recv(8))[0]
            payload = self._recv(n)
            if op == 9:
                self.send(10, payload)
                continue
            if op == 10:
                continue
            if op == 8:
                raise ConnectionError("server closed")
            if op == 0:
                data += payload
            else:
                data = payload
                first = op
            if b0 & 0x80:
                return first, data

    def recv_json(self, want=None, timeout=10):
        """The next text frame (of type `want`, skipping others)."""
        end = time.time() + timeout
        while time.time() < end:
            self.sock.settimeout(max(0.05, end - time.time()))
            op, d = self.recv()
            if op == 1:
                v = json.loads(d)
                if v.get("t") in ("hello", "state"):
                    self.last_state = v["state"]
                if want is None or v.get("t") == want:
                    return v
        raise TimeoutError(f"no {want} frame")

    def close(self):
        try:
            self.send(8, b"")
            self.sock.close()
        except OSError:
            pass


def login(host, port, token):
    """Trade the token for the session cookie ("name=value")."""
    c = http.client.HTTPConnection(host, port, timeout=10)
    c.request("GET", f"/?t={token}")
    r = c.getresponse()
    r.read()
    if r.status != 303:
        raise RuntimeError(f"login: HTTP {r.status}")
    return r.getheader("Set-Cookie").split(";")[0]
