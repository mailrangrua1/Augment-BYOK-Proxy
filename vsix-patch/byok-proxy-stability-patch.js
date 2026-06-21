// === BYOK Proxy Stability Patch ===
// marker: __augment_byok_stability_patch
// Fix cho v0.890.1+:
// 1. Đảm bảo module.exports được restore đúng cách nếu inject-code.txt overwrite nó
// 2. ACTIVE reload guard — wrap vscode.commands.executeCommand để chặn rapid reload
// 3. Suppress uncaught "Failed to extract tenant ID" errors
//
// NOTE: aee() / IPe() localhost fix được xử lý trực tiếp bởi patch_stability_fixes()
//       trong repack_vsix.py (thay thế trực tiếp trong bundle code).

(function () {
  "use strict";

  const MARKER = "__augment_byok_stability_patch";
  if (globalThis && globalThis[MARKER]) return;
  try { if (globalThis) globalThis[MARKER] = true; } catch (_) { }

  // === Patch 1: Ensure module.exports integrity ===
  (function ensureModuleExportsIntegrity() {
    try {
      if (typeof module !== "object" || !module) return;
      if (typeof exports !== "object" || !exports) return;
      if (module.exports && module.exports !== exports) {
        const keys = Object.keys(module.exports || {});
        const isInjectWrapper = (
          keys.includes("createExtensionWrapper") ||
          keys.includes("processInterceptedRequest") ||
          keys.includes("wrapAsExtension")
        );
        if (isInjectWrapper) {
          module.exports = exports;
        }
      }
    } catch (_) { }
  })();

  // === Patch 2: ACTIVE reload guard ===
  // Wrap vscode.commands.executeCommand to intercept and debounce
  // 'workbench.action.reloadWindow' calls that cause extension reset loops.
  (function installActiveReloadGuard() {
    try {
      const GUARD_KEY = "__augment_byok_reload_guard";
      if (globalThis[GUARD_KEY]) return;

      const guard = {
        lastReloadTime: 0,
        reloadCount: 0,
        blockedCount: 0,
        MIN_RELOAD_INTERVAL_MS: 5000,
        MAX_RELOADS_PER_MINUTE: 5,
        reloadTimestamps: [],
        shouldAllow: function () {
          const now = Date.now();
          // Check minimum interval
          if (now - this.lastReloadTime < this.MIN_RELOAD_INTERVAL_MS) {
            this.blockedCount++;
            console.warn(
              "[BYOK Stability] Blocked rapid reload (interval: " +
              (now - this.lastReloadTime) + "ms, blocked total: " + this.blockedCount + ")"
            );
            return false;
          }
          // Check rate limit (max N per minute)
          this.reloadTimestamps = this.reloadTimestamps.filter(function (t) { return now - t < 60000; });
          if (this.reloadTimestamps.length >= this.MAX_RELOADS_PER_MINUTE) {
            this.blockedCount++;
            console.warn(
              "[BYOK Stability] Blocked reload (rate limit: " +
              this.reloadTimestamps.length + "/" + this.MAX_RELOADS_PER_MINUTE + " per min)"
            );
            return false;
          }
          this.lastReloadTime = now;
          this.reloadCount++;
          this.reloadTimestamps.push(now);
          return true;
        }
      };
      globalThis[GUARD_KEY] = guard;

      // Wrap vscode.commands.executeCommand to intercept reload commands
      var vscode;
      try { vscode = require("vscode"); } catch (_) { }
      if (vscode && vscode.commands && typeof vscode.commands.executeCommand === "function") {
        var origExec = vscode.commands.executeCommand.bind(vscode.commands);
        vscode.commands.executeCommand = function (command) {
          if (command === "workbench.action.reloadWindow") {
            if (!guard.shouldAllow()) {
              console.warn("[BYOK Stability] Reload command suppressed");
              return Promise.resolve();
            }
            console.info("[BYOK Stability] Allowing reload #" + guard.reloadCount);
          }
          return origExec.apply(vscode.commands, arguments);
        };
      }
    } catch (_) { }
  })();

  // === Patch 3: Suppress tenant ID extraction errors ===
  // If aee()/IPe() patches in repack_vsix.py didn't match (version mismatch),
  // catch the error at process level to prevent crash.
  (function suppressTenantIdErrors() {
    try {
      if (typeof process === "undefined" || !process.on) return;
      var origListeners = process.listeners("uncaughtException").slice();
      process.on("uncaughtException", function (err) {
        if (err && typeof err.message === "string" &&
            err.message.indexOf("Failed to extract tenant ID") !== -1) {
          console.warn("[BYOK Stability] Suppressed tenant ID error: " + err.message);
          return; // swallow this specific error
        }
        // Re-throw other errors to original handlers
        for (var i = 0; i < origListeners.length; i++) {
          try { origListeners[i](err); } catch (_) { }
        }
      });
    } catch (_) { }
  })();

})();;

// === BYOK Proxy Stability Patch End ===
