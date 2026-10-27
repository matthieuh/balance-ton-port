#!/usr/bin/env python3
"""A tiny dev server for the `btp run` tests.

Port from `--port N`, `--port=N` or `-p N`, else `PORT`, else 8000. Prints
`serve.py listening on <port> pid <pid>` once it listens. With
`SERVE_EXIT_AFTER` (seconds) it exits by itself after that long, with code
`SERVE_EXIT_CODE` (default 0).
"""
import http.server
import os
import socketserver
import sys
import threading


def port_from(args):
    port = None
    for i, a in enumerate(args):
        if a in ("--port", "-p") and i + 1 < len(args):
            port = int(args[i + 1])
        elif a.startswith("--port="):
            port = int(a.split("=", 1)[1])
    if port is None:
        port = int(os.environ.get("PORT", "8000"))
    return port


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok\n"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Server(http.server.HTTPServer):
    # HTTPServer.server_bind looks the host up in DNS (getfqdn), which can
    # hang for seconds on a CI runner; the name is never used here.
    def server_bind(self):
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = "localhost", self.server_address[1]


def main():
    port = port_from(sys.argv[1:])
    server = Server(("127.0.0.1", port), Handler)
    print(f"serve.py listening on {port} pid {os.getpid()}", flush=True)
    after = os.environ.get("SERVE_EXIT_AFTER")
    if after:
        code = int(os.environ.get("SERVE_EXIT_CODE", "0"))
        threading.Timer(float(after), lambda: os._exit(code)).start()
    server.serve_forever()


if __name__ == "__main__":
    main()
