// Sidecar e2e fixture plugin.
// Minimal CommonJS module exposing the JSON-RPC methods the sidecar
// expects. Used by tests/sidecar_e2e.rs.

'use strict';

const PLUGIN_ID = '00000000-0000-0000-0000-000000000001';

module.exports = {
  async initialize({ manifest }) {
    return {
      pluginId: PLUGIN_ID,
      manifest,
      initializedAt: new Date().toISOString(),
    };
  },

  async health() {
    return { status: 'ok' };
  },

  async shutdown() {
    // Allow process to exit cleanly
    return { ok: true };
  },
};
