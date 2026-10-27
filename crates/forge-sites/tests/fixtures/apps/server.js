'use strict';
// Test app of M1-E4-T5: HTTP + WebSocket echo using only Node's built-in modules.
// It stands in for the dev server of a framework (Next.js/Vite HMR is an upgrade request on
// the same port). Listens on 127.0.0.1:$PORT (or the first argument).
const http = require('http');
const crypto = require('crypto');

const port = Number(process.env.PORT || process.argv[2]);
if (!Number.isInteger(port)) {
  console.error('no PORT');
  process.exit(2);
}
const GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

const server = http.createServer((req, res) => {
  if (req.url === '/env') {
    const body = JSON.stringify({
      version: process.version,
      port,
      cwd: process.cwd(),
      argv: process.argv.slice(2),
      env: {
        PORT: process.env.PORT,
        FORGEBOX_SITE: process.env.FORGEBOX_SITE,
        NODE_EXTRA_CA_CERTS: process.env.NODE_EXTRA_CA_CERTS,
        npm_config_prefix: process.env.npm_config_prefix,
        APP_ENV: process.env.APP_ENV,
        PATH: process.env.PATH,
      },
      host: req.headers.host,
      forwardedProto: req.headers['x-forwarded-proto'],
      forgeboxSite: req.headers['forgebox_site'],
    });
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(body);
    return;
  }
  res.writeHead(200, { 'content-type': 'text/plain' });
  res.end('hello from forgebox app\n');
});

function frame(opcode, payload) {
  const len = payload.length;
  let head;
  if (len < 126) {
    head = Buffer.from([0x80 | opcode, len]);
  } else if (len < 65536) {
    head = Buffer.from([0x80 | opcode, 126, len >> 8, len & 0xff]);
  } else {
    head = Buffer.alloc(10);
    head[0] = 0x80 | opcode;
    head[1] = 127;
    head.writeUInt32BE(len, 6);
  }
  return Buffer.concat([head, payload]);
}

server.on('upgrade', (req, socket) => {
  const key = req.headers['sec-websocket-key'];
  if (!key) {
    socket.destroy();
    return;
  }
  const accept = crypto.createHash('sha1').update(key + GUID).digest('base64');
  socket.write(
    'HTTP/1.1 101 Switching Protocols\r\n' +
      'Upgrade: websocket\r\nConnection: Upgrade\r\n' +
      `Sec-WebSocket-Accept: ${accept}\r\n\r\n`
  );
  let buffer = Buffer.alloc(0);
  socket.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      if (buffer.length < 2) return;
      const opcode = buffer[0] & 0x0f;
      const masked = (buffer[1] & 0x80) !== 0;
      let len = buffer[1] & 0x7f;
      let offset = 2;
      if (len === 126) {
        if (buffer.length < 4) return;
        len = buffer.readUInt16BE(2);
        offset = 4;
      } else if (len === 127) {
        if (buffer.length < 10) return;
        len = buffer.readUInt32BE(6);
        offset = 10;
      }
      const maskLen = masked ? 4 : 0;
      if (buffer.length < offset + maskLen + len) return;
      const mask = buffer.subarray(offset, offset + maskLen);
      const payload = Buffer.from(buffer.subarray(offset + maskLen, offset + maskLen + len));
      if (masked) for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i % 4];
      buffer = buffer.subarray(offset + maskLen + len);
      if (opcode === 8) {
        socket.end(frame(8, Buffer.alloc(0)));
        return;
      }
      if (opcode === 9) socket.write(frame(10, payload));
      else if (opcode === 1 || opcode === 2) socket.write(frame(opcode, payload));
    }
  });
  socket.on('error', () => {});
});

server.listen(port, '127.0.0.1', () => console.log(`listening on ${port}`));
process.on('SIGINT', () => process.exit(0));
