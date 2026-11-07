#!/usr/bin/env python3
"""LSP の診断の遅延（M6 PoC6、docs/poc/poc6-results.md）: `wys lsp` に同じ全文の didChange を N 回送り、
publishDiagnostics が届くまでの時間の最速値・中央値・p95 を出す。絶対値だけを記録する（倍率は書かない）。

使い方: benches/lsp_latency.py target/release/wys <file.wys>... （N は環境変数 RUNS、既定 50）
"""
import json, os, statistics, subprocess, sys, time

def send(p, v):
    b = json.dumps(v).encode()
    p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    p.stdin.flush()

def recv(p):
    n = 0
    while True:
        line = p.stdout.readline().decode().strip()
        if not line:
            break
        if line.startswith("Content-Length:"):
            n = int(line.split()[1])
    return json.loads(p.stdout.read(n))

def main():
    wys, files = sys.argv[1], sys.argv[2:]
    runs = int(os.environ.get("RUNS", "50"))
    p = subprocess.Popen([wys, "lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    send(p, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}})
    recv(p)
    send(p, {"jsonrpc": "2.0", "method": "initialized", "params": {}})
    for f in files:
        f = os.path.abspath(f)
        uri = "file://" + f
        text = open(f).read()
        send(p, {"jsonrpc": "2.0", "method": "textDocument/didOpen",
                 "params": {"textDocument": {"uri": uri, "languageId": "wys", "version": 1, "text": text}}})
        times = []
        for i in range(runs):
            # 診断が変わらない文書には配り直さないので、型エラーのある関数を足す・外すを交互に行い、
            # 毎回フロントエンドを全部回して診断を配らせる
            probe = "\ndef wys_lsp_probe() -> int:\n    return \"x\"\n"
            changed = text + (probe if i % 2 == 0 else "")
            t = time.perf_counter()
            send(p, {"jsonrpc": "2.0", "method": "textDocument/didChange",
                     "params": {"textDocument": {"uri": uri, "version": i + 2}, "contentChanges": [{"text": changed}]}})
            m = recv(p)
            while m.get("params", {}).get("uri") != uri:
                m = recv(p)
            times.append(time.perf_counter() - t)
        times.sort()
        p95 = times[min(len(times) - 1, int(len(times) * 0.95))]
        print(f"{f}: {len(text.encode())} bytes, best {times[0]*1000:.2f} ms, median {statistics.median(times)*1000:.2f} ms, p95 {p95*1000:.2f} ms")
    send(p, {"jsonrpc": "2.0", "id": 2, "method": "shutdown"})
    recv(p)
    send(p, {"jsonrpc": "2.0", "method": "exit"})
    p.stdin.close()
    p.wait()

main()
