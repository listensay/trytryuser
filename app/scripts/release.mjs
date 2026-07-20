#!/usr/bin/env node
/**
 * 版本发布脚本
 *
 * 用法（在 app 目录下执行）：
 *   npm run release -- 0.0.1
 *   npm run release -- --0.0.1     （两种写法都支持）
 *   npm run release -- 0.0.1 --dry-run   （只预览，不实际修改/推送）
 *
 * 功能：
 *   1. 同步更新 package.json / tauri.conf.json / Cargo.toml 的版本号
 *   2. git commit + 打 tag（v0.0.1）
 *   3. push 到 GitHub，触发 .github/workflows/release.yml 自动打包发布 Release
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { execSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(__dirname, '..');

// ---------- 解析参数 ----------
const rawArgs = process.argv.slice(2);
const dryRun = rawArgs.includes('--dry-run');
const version = rawArgs
  .filter((a) => a !== '--dry-run')
  .map((a) => a.replace(/^-+/, '')) // 兼容 --0.0.1 写法
  .find((a) => a.length > 0);

if (!version) {
  console.error('❌ 请提供版本号，例如：npm run release -- 0.0.1');
  process.exit(1);
}
if (!/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(version)) {
  console.error(`❌ 版本号格式不正确："${version}"，应为 x.y.z 格式，例如 0.0.1`);
  process.exit(1);
}

const tag = `v${version}`;
console.log(`🚀 准备发布版本 ${tag}${dryRun ? '（dry-run 预览模式）' : ''}\n`);

const run = (cmd, opts = {}) => {
  console.log(`  $ ${cmd}`);
  if (!dryRun) execSync(cmd, { stdio: 'inherit', cwd: appDir, ...opts });
};

// ---------- 前置检查 ----------
try {
  const existing = execSync(`git tag -l ${tag}`, { cwd: appDir }).toString().trim();
  if (existing) {
    console.error(`❌ tag ${tag} 已存在，请换一个版本号`);
    process.exit(1);
  }
} catch {
  /* 忽略 */
}

// ---------- 1. 更新版本号 ----------
const files = {
  pkg: path.join(appDir, 'package.json'),
  conf: path.join(appDir, 'src-tauri', 'tauri.conf.json'),
  cargo: path.join(appDir, 'src-tauri', 'Cargo.toml'),
  lock: path.join(appDir, 'src-tauri', 'Cargo.lock'),
};

// package.json
const pkg = JSON.parse(readFileSync(files.pkg, 'utf8'));
console.log(`📦 package.json:     ${pkg.version} -> ${version}`);
pkg.version = version;
if (!dryRun) writeFileSync(files.pkg, JSON.stringify(pkg, null, 2) + '\n');

// package-lock.json（保持与 package.json 同步）
const npmLockPath = path.join(appDir, 'package-lock.json');
try {
  const npmLock = JSON.parse(readFileSync(npmLockPath, 'utf8'));
  npmLock.name = pkg.name;
  npmLock.version = version;
  if (npmLock.packages && npmLock.packages['']) {
    npmLock.packages[''].name = pkg.name;
    npmLock.packages[''].version = version;
  }
  if (!dryRun) writeFileSync(npmLockPath, JSON.stringify(npmLock, null, 2) + '\n');
  console.log(`📦 package-lock.json: -> ${version}`);
} catch {
  console.log('⚠️  未找到 package-lock.json，跳过');
}

// tauri.conf.json
const conf = JSON.parse(readFileSync(files.conf, 'utf8'));
console.log(`📦 tauri.conf.json:  ${conf.version} -> ${version}`);
conf.version = version;
if (!dryRun) writeFileSync(files.conf, JSON.stringify(conf, null, 2) + '\n');

// Cargo.toml（只改 [package] 段的第一个 version）
const cargo = readFileSync(files.cargo, 'utf8');
const newCargo = cargo.replace(/^version\s*=\s*"[^"]*"/m, `version = "${version}"`);
console.log(`📦 Cargo.toml:       version -> ${version}`);
if (!dryRun) writeFileSync(files.cargo, newCargo);

// Cargo.lock（同步本包版本，避免 CI 中 lock 文件变动）
try {
  const cargoName = cargo.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
  if (cargoName) {
    const lock = readFileSync(files.lock, 'utf8');
    const re = new RegExp(`(name = "${cargoName}"\\nversion = )"[^"]*"`);
    if (!dryRun) writeFileSync(files.lock, lock.replace(re, `$1"${version}"`));
    console.log(`📦 Cargo.lock:       ${cargoName} -> ${version}`);
  }
} catch {
  console.log('⚠️  未找到 Cargo.lock，跳过');
}

// ---------- 2. 提交 + 打 tag + 推送 ----------
console.log('');
run(`git add package.json package-lock.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock`);
run(`git commit -m "chore: release ${tag}"`);
run(`git tag -a ${tag} -m "release ${tag}"`);
run(`git push origin HEAD`);
run(`git push origin ${tag}`);

console.log(`\n✅ 完成！tag ${tag} 已推送。`);
console.log('   GitHub Actions 会自动打包 Windows / macOS / Linux 安装包并发布 Release：');
console.log('   https://github.com/listensay/getUserVip/actions');
