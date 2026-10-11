import json, subprocess, sys
class Client:
    def __init__(self, exe):
        self.p = subprocess.Popen([exe, 'mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open('server.log','w'), text=True)
        self.i = 0
        self.req('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'e2e', 'version': '0'}})
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
    def send(self, m):
        self.p.stdin.write(json.dumps(m) + '\n'); self.p.stdin.flush()
    def req(self, method, params=None):
        self.i += 1
        self.send({'jsonrpc': '2.0', 'id': self.i, 'method': method, 'params': params or {}})
        while True:
            m = json.loads(self.p.stdout.readline())
            if m.get('id') == self.i: break
        if 'error' in m: raise RuntimeError(m['error'])
        return m['result']
    def call(self, _tool, **args):
        name = _tool
        r = self.req('tools/call', {'name': name, 'arguments': args})
        if r.get('isError'): raise RuntimeError(f"{name}: {r['content'][0]['text']}")
        return r.get('structuredContent') or json.loads(r['content'][0]['text']) if r.get('content') else r.get('structuredContent')
