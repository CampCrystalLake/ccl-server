"use strict";
const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const net = require("node:net");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const httpPort = 18451;
const tcpPort = 19451;
const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "klay-node-test-"));
const child = spawn(process.execPath, ["server.js"], {
  cwd: process.cwd(),
  env: {
    ...process.env,
    HOST: "127.0.0.1",
    PORT: String(httpPort),
    TCP_HOST: "127.0.0.1",
    TCP_PORT: String(tcpPort),
    DATABASE_URL: path.join(tempDir, "test.db"),
    COOKIE_SECURE: "false",
    PUBLIC_URL: "http://127.0.0.1:" + httpPort
  },
  stdio: ["ignore", "pipe", "pipe"]
});
let logs = "";
child.stdout.on("data", chunk => { logs += chunk.toString(); });
child.stderr.on("data", chunk => { logs += chunk.toString(); });

function delay(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }
async function waitForServer() {
  for (let i = 0; i < 80; i++) {
    if (child.exitCode !== null) throw new Error("Backend exited early:\n" + logs);
    try {
      const response = await fetch("http://127.0.0.1:" + httpPort + "/health");
      if (response.ok) return;
    } catch {}
    await delay(100);
  }
  throw new Error("Backend did not become healthy:\n" + logs);
}
function makeFrame(id, requestId, payload) {
  const body = Buffer.from(JSON.stringify(payload));
  const header = Buffer.alloc(15);
  header.write("KLAY", 0, 4, "ascii");
  header[4] = 1;
  header.writeUInt16BE(id, 5);
  header.writeUInt32BE(requestId, 7);
  header.writeUInt32BE(body.length, 11);
  return Buffer.concat([header, body]);
}
function sendTcp(socket, id, requestId, payload) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);
    const timer = setTimeout(() => {
      socket.off("data", onData);
      reject(new Error("TCP response timeout"));
    }, 4000);
    function onData(chunk) {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length < 15) return;
      const length = buffer.readUInt32BE(11);
      if (buffer.length < 15 + length) return;
      clearTimeout(timer);
      socket.off("data", onData);
      assert.equal(buffer.toString("ascii", 0, 4), "KLAY");
      assert.equal(buffer.readUInt16BE(5), id | 0x8000);
      assert.equal(buffer.readUInt32BE(7), requestId);
      resolve(JSON.parse(buffer.subarray(15, 15 + length).toString("utf8")));
    }
    socket.on("data", onData);
    socket.write(makeFrame(id, requestId, payload));
  });
}
async function main() {
  try {
    await waitForServer();
    const health = await (await fetch("http://127.0.0.1:" + httpPort + "/health")).json();
    assert.equal(health.status, "ok");
    assert.equal(health.runtime, "nodejs");

    const loginResponse = await fetch("http://127.0.0.1:" + httpPort + "/api/v1/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ display_name: "KLAY_Test" })
    });
    const login = await loginResponse.json();
    assert.equal(login.success, true);
    assert.ok(login.token && login.userId);

    const validation = await (await fetch("http://127.0.0.1:" + httpPort + "/api/v1/session/validate", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token: login.token })
    })).json();
    assert.equal(validation.valid, true);

    const lobbyResponse = await fetch("http://127.0.0.1:" + httpPort + "/api/v1/lobbies/create", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token: login.token, name: "Smoke Test Lobby" })
    });
    const lobby = await lobbyResponse.json();
    assert.equal(lobby.success, true);
    assert.equal(lobby.players, 1);

    const socket = net.createConnection({ host: "127.0.0.1", port: tcpPort });
    await new Promise((resolve, reject) => { socket.once("connect", resolve); socket.once("error", reject); });
    const auth = await sendTcp(socket, 1, 11, { type: "Authenticate", player_id: login.userId, token: login.token });
    assert.equal(auth.type, "Authenticated");
    const created = await sendTcp(socket, 3, 12, { type: "CreateSession" });
    assert.equal(created.type, "SessionCreated");
    const ready = await sendTcp(socket, 6, 13, { type: "SetReady", ready: true });
    assert.equal(ready.type, "ReadyChanged");
    const started = await sendTcp(socket, 7, 14, { type: "StartSession" });
    assert.equal(started.type, "SessionStarted");
    socket.destroy();

    console.log("Node.js smoke tests passed: health, login, session validation, lobby creation, TCP auth/session/ready/start.");
  } finally {
    child.kill("SIGTERM");
    await Promise.race([new Promise(resolve => child.once("exit", resolve)), delay(2000)]);
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
