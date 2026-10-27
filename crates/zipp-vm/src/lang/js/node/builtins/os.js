// node:os — what a program asks of its machine, from the environment.
(function (host) {
  "use strict";
  const env = () => (globalThis.process ? process.env : {});
  const win = host.platform === "win32";
  return {
    EOL: win ? "\r\n" : "\n",
    devNull: win ? "\\\\.\\nul" : "/dev/null",
    constants: { signals: {}, errno: {}, priority: {} },
    platform: () => host.platform,
    type: () => (win ? "Windows_NT" : host.platform === "darwin" ? "Darwin" : "Linux"),
    arch: () => host.arch,
    machine: () => (host.arch === "x64" ? "x86_64" : host.arch),
    release: () => "",
    version: () => "",
    hostname: () => env().COMPUTERNAME || env().HOSTNAME || "localhost",
    homedir: () => env().USERPROFILE || env().HOME || "",
    tmpdir: () => {
      const t = env().TMPDIR || env().TMP || env().TEMP || (win ? "C:\\Windows\\Temp" : "/tmp");
      return t.length > 1 && (t.endsWith("/") || t.endsWith("\\")) ? t.slice(0, -1) : t;
    },
    endianness: () => "LE",
    cpus: () => [{ model: "unknown", speed: 0, times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 } }],
    availableParallelism: () => 1,
    totalmem: () => 0,
    freemem: () => 0,
    uptime: () => (globalThis.process ? process.uptime() : 0),
    loadavg: () => [0, 0, 0],
    networkInterfaces: () => ({}),
    userInfo: () => ({ username: env().USERNAME || env().USER || "", uid: -1, gid: -1, shell: env().SHELL || null, homedir: env().USERPROFILE || env().HOME || "" }),
    getPriority: () => 0,
    setPriority: () => {},
  };
})
