#!/usr/bin/env node
// paperclip-plugin-sidecar — R877 JS 插件 sidecar proxy
//
// 这是 Rust host 与 Node paperclip 插件之间的桥梁：
// - 从 argv 接收 --plugin-id + --manifest 路径
// - 用 node:vm 加载插件（保留 Node paperclip 的隔离模型）
// - 在 stdio 上与 host 走 JSON-RPC 2.0 协议
//
// Usage:
//   node paperclip-plugin-sidecar.mjs --plugin-id <UUID> --manifest <path>
//
// 协议方法（host → sidecar，sidecar 转发到插件）：
//   initialize, health, shutdown, validateConfig, configChanged,
//   onEvent, runJob, handleWebhook, handleApiRequest, getData,
//   performAction, executeTool, detectExternalObjects, resolveExternalObject,
//   refreshExternalObjects
//   + 9 个 environment* 方法
//
// 协议方法（sidecar → host，从插件 onEvent 派发）：
//   progress, log, emitEvent, getState, setState, dataQuery,
//   dataMutate, toolInvoke, activityLog, notify
//
// Protocol 详见 paperclip-rs/crates/pc-plugin-protocol/src/methods.rs

'use strict';

// Bridge to allow require() in this ESM script (.mjs). Without this,
// Node ESM scope rejects CommonJS `require` with "ReferenceError:
// require is not defined". Using createRequire keeps the script
// runnable as `.mjs` (preserving the original file extension that other
// tools — Cargo include, IDE plugins — key off) while still letting the
// body use CommonJS-style imports.
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);

const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const HOST_TO_WORKER_METHODS = new Set([
  'initialize', 'health', 'shutdown', 'validateConfig', 'configChanged',
  'onEvent', 'runJob', 'handleWebhook', 'handleApiRequest', 'getData',
  'performAction', 'executeTool', 'detectExternalObjects', 'resolveExternalObject',
  'refreshExternalObjects',
  'environmentValidateConfig', 'environmentProbe', 'environmentAcquireLease',
  'environmentResumeLease', 'environmentReleaseLease', 'environmentRealizeWorkspace',
  'environmentDisposeWorkspace', 'environmentTick', 'environmentStop',
]);

const WORKER_TO_HOST_METHODS = new Set([
  'progress', 'log', 'emitEvent', 'getState', 'setState', 'dataQuery',
  'dataMutate', 'toolInvoke', 'activityLog', 'notify',
]);

// ============================================================================
// argv parsing
// ============================================================================
function parseArgs(argv) {
  const args = { pluginId: null, manifest: null };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--plugin-id' && i + 1 < argv.length) {
      args.pluginId = argv[++i];
    } else if (arg === '--manifest' && i + 1 < argv.length) {
      args.manifest = argv[++i];
    }
  }
  if (!args.pluginId || !args.manifest) {
    process.stderr.write('[sidecar] missing --plugin-id or --manifest\n');
    process.exit(2);
  }
  return args;
}

// ============================================================================
// JSON-RPC envelope
// ============================================================================
let nextRequestId = 1;

function rpcResponse(id, result) {
  return JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n';
}

function rpcError(id, code, message, data) {
  const err = { code, message };
  if (data !== undefined) err.data = data;
  return JSON.stringify({ jsonrpc: '2.0', id, error: err }) + '\n';
}

// Standard JSON-RPC 2.0 error codes
const RPC_PARSE_ERROR = -32700;
const RPC_INVALID_REQUEST = -32600;
const RPC_METHOD_NOT_FOUND = -32601;
const RPC_INVALID_PARAMS = -32602;
const RPC_INTERNAL_ERROR = -32603;

// ============================================================================
// Plugin loader — node:vm sandbox
// ============================================================================
function loadPlugin(manifestPath, pluginId) {
  const manifestRaw = fs.readFileSync(manifestPath, 'utf8');
  const manifest = JSON.parse(manifestRaw);

  if (manifest.id !== pluginId) {
    throw new Error(
      `manifest id "${manifest.id}" does not match --plugin-id "${pluginId}"`
    );
  }
  if (!manifest.entry) {
    throw new Error('manifest.entry is required');
  }

  const pluginDir = path.dirname(path.resolve(manifestPath));
  const entryPath = path.resolve(pluginDir, manifest.entry);

  // Capability check: ensure plugin does not declare capabilities beyond
  // what node:vm can provide.
  const safeCapabilities = new Set([
    'jobs', 'events', 'data', 'actions', 'tools', 'webhooks',
    'ui', 'externalObjects', 'environments', 'access',
  ]);
  for (const cap of manifest.capabilities ?? []) {
    if (!safeCapabilities.has(cap.kind)) {
      throw new Error(`unsupported capability kind: ${cap.kind}`);
    }
  }

  // Sandboxed context with safe builtins.
  const sandbox = {
    console,
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
    setImmediate,
    clearImmediate,
    Buffer,
    process: {
      env: { ...process.env },
      version: process.version,
      platform: process.platform,
    },
    URL,
    URLSearchParams,
    TextEncoder,
    TextDecoder,
    // Plugin-specific APIs exposed via sandbox:
    manifest,
    pluginId,
  };

  vm.createContext(sandbox);
  const script = new vm.Script(
    fs.readFileSync(entryPath, 'utf8'),
    { filename: entryPath }
  );
  script.runInContext(sandbox, { timeout: 2000 });

  if (typeof sandbox.module === 'undefined' || typeof sandbox.module.exports === 'undefined') {
    throw new Error(
      `plugin entry did not export a module. Expected: module.exports = { initialize, health, runJob, ... }`
    );
  }

  return { plugin: sandbox.module.exports, manifest };
}

// ============================================================================
// RPC dispatch loop
// ============================================================================
function setupStdio(plugin, manifest, pluginId) {
  let buf = '';

  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    buf += chunk;
    let nl;
    while ((nl = buf.indexOf('\n')) !== -1) {
      const line = buf.slice(0, nl);
      buf = buf.slice(nl + 1);
      if (line.trim()) handleRequest(line);
    }
  });

  process.stdin.on('end', () => {
    process.exit(0);
  });

  async function handleRequest(line) {
    let req;
    try {
      req = JSON.parse(line);
    } catch (e) {
      process.stdout.write(rpcError(null, RPC_PARSE_ERROR, 'parse error'));
      return;
    }

    if (req.jsonrpc !== '2.0' || typeof req.method !== 'string') {
      process.stdout.write(rpcError(req.id ?? null, RPC_INVALID_REQUEST, 'invalid request'));
      return;
    }

    const method = req.method;
    const id = req.id ?? null;

    if (!HOST_TO_WORKER_METHODS.has(method)) {
      process.stdout.write(rpcError(id, RPC_METHOD_NOT_FOUND, `method not found: ${method}`));
      return;
    }

    try {
      const result = await dispatchToPlugin(method, req.params ?? {});
      process.stdout.write(rpcResponse(id, result ?? null));
    } catch (err) {
      const message = err && err.message ? err.message : String(err);
      const data = err && err.stack ? { stack: err.stack } : undefined;
      process.stdout.write(rpcError(id, RPC_INTERNAL_ERROR, message, data));
    }
  }

  async function dispatchToPlugin(method, params) {
    switch (method) {
      case 'initialize':
        if (typeof plugin.initialize !== 'function') {
          throw new Error('plugin does not implement initialize');
        }
        return await plugin.initialize({
          pluginId,
          manifest,
          params: params ?? {},
        });

      case 'health':
        if (typeof plugin.health === 'function') {
          return await plugin.health(params);
        }
        return { status: 'ok' };

      case 'shutdown':
        if (typeof plugin.shutdown === 'function') {
          await plugin.shutdown(params);
        }
        // Allow the process to exit naturally.
        setImmediate(() => process.exit(0));
        return { ok: true };

      default:
        // Pass through to plugin if it implements the method.
        if (typeof plugin[method] !== 'function') {
          throw new Error(`plugin does not implement ${method}`);
        }
        return await plugin[method](params);
    }
  }
}

// ============================================================================
// Worker → Host RPC (forward plugin callbacks via stdio)
// ============================================================================
// Plugins can invoke host services via a global function we inject into
// the sandbox:
function injectHostBridge(sandbox) {
  sandbox.__paperclip_host = {
    progress: (params) => sendToHost('progress', params),
    log: (params) => sendToHost('log', params),
    emitEvent: (params) => sendToHost('emitEvent', params),
    getState: (params) => sendToHost('getState', params),
    setState: (params) => sendToHost('setState', params),
    dataQuery: (params) => sendToHost('dataQuery', params),
    dataMutate: (params) => sendToHost('dataMutate', params),
    toolInvoke: (params) => sendToHost('toolInvoke', params),
    activityLog: (params) => sendToHost('activityLog', params),
    notify: (params) => sendToHost('notify', params),
  };
}

const pendingHostRequests = new Map();

function sendToHost(method, params) {
  return new Promise((resolve, reject) => {
    if (!WORKER_TO_HOST_METHODS.has(method)) {
      reject(new Error(`unknown worker→host method: ${method}`));
      return;
    }
    const id = `sidecar-${nextRequestId++}`;
    pendingHostRequests.set(id, { resolve, reject, method });
    process.stdout.write(
      JSON.stringify({ jsonrpc: '2.0', id, method, params: params ?? {} }) + '\n'
    );
  });
}

function handleHostResponse(line) {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch (e) {
    return; // not JSON; ignore
  }
  if (typeof msg.id !== 'string' || !msg.id.startsWith('sidecar-')) return;
  const pending = pendingHostRequests.get(msg.id);
  if (!pending) return;
  pendingHostRequests.delete(msg.id);
  if (msg.error) {
    pending.reject(new Error(msg.error.message || 'host error'));
  } else {
    pending.resolve(msg.result);
  }
}

// ============================================================================
// Main
// ============================================================================
async function main() {
  const args = parseArgs(process.argv.slice(2));
  process.stderr.write(`[sidecar] starting for plugin ${args.pluginId}\n`);

  let plugin, manifest;
  try {
    ({ plugin, manifest } = loadPlugin(args.manifest, args.pluginId));
  } catch (err) {
    process.stderr.write(`[sidecar] load failed: ${err.message}\n`);
    process.exit(3);
  }

  // Intercept outgoing JSON-RPC: lines with method = worker→host go to host,
  // lines with id matching pending go back to the plugin caller.
  const originalStdoutWrite = process.stdout.write.bind(process.stdout);
  process.stdout.write = (chunk, ...args) => {
    const line = typeof chunk === 'string' ? chunk : chunk.toString('utf8');
    if (line.trim().endsWith('}')) {
      try {
        const obj = JSON.parse(line);
        if (obj.method && WORKER_TO_HOST_METHODS.has(obj.method)) {
          // Forward to host (this is a sidecar → host request).
          // The host will respond with { id: ..., result/error } on stdin.
          return originalStdoutWrite(chunk, ...args);
        }
        if (obj.id && pendingHostRequests.has(obj.id)) {
          handleHostResponse(line);
          return true; // don't write — handled internally
        }
      } catch (_) {
        // not JSON; pass through
      }
    }
    return originalStdoutWrite(chunk, ...args);
  };

  setupStdio(plugin, manifest, args.pluginId);
  process.stderr.write(`[sidecar] ready\n`);
}

main().catch((err) => {
  process.stderr.write(`[sidecar] fatal: ${err.stack || err.message}\n`);
  process.exit(1);
});
