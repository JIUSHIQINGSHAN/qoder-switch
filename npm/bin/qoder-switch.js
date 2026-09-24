#!/usr/bin/env node
// qoder-switch npm 入口：打印 webui 服务端的就绪启动命令。
//
// 为什么不直接 spawn：本仓库的写入安全门（Mimosa）无条件拦截 JS 里的子进程调用，
// 哪怕参数全是包内字面量。于是这里退而求其次——把可直接复制运行的绝对路径命令
// 打印出来；webui 形态的用户本来就是开发者，直接运行那行命令即可。
// postinstall（scripts/install.js）已把平台二进制就位到 bin/。
const fs = require("fs");
const path = require("path");

const FILE = {
  "win32-x64": "qs-switch-server.exe",
  "darwin-arm64": "qs-switch-server",
  "darwin-x64": "qs-switch-server",
}[`${process.platform}-${process.arch}`];

if (!FILE) {
  console.error(
    `qoder-switch: 暂无 ${process.platform}-${process.arch} 平台包` +
      `（当前发布 win32-x64 / darwin-arm64 / darwin-x64）`
  );
  process.exit(1);
}

const exe = path.join(__dirname, "..", "bin", FILE);
const dist = path.join(__dirname, "..", "webui_dist");

if (!fs.existsSync(exe)) {
  console.error("qoder-switch: 未找到平台二进制，请重新安装（npm install -g qoder-switch）");
  process.exit(1);
}
if (!fs.existsSync(path.join(dist, "index.html"))) {
  console.error("qoder-switch: 包内缺少 webui_dist（发布包损坏），请重新安装");
  process.exit(1);
}

const userArgs = process.argv
  .slice(2)
  .map((arg) => (arg.includes(" ") || arg.includes('"') ? `"${arg.replace(/"/g, '\\"')}"` : arg))
  .join(" ");
const extra = userArgs ? ` ${userArgs}` : "";

console.log("Qoder Switch webui 已就绪。运行下面这行启动（默认 127.0.0.1:57891）：");
console.log(`  "${exe}" --dist "${dist}"${extra}`);
console.log("自定义端口：在命令后追加 --port <1-65535>。浏览器打开 http://127.0.0.1:57891");
