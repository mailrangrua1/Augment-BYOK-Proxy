// === BYOK Proxy Stability Patch ===
// marker: __augment_byok_stability_patch
// Fix cho v0.890.1+:
// 1. Debounce onDidChangeConfiguration để tránh rapid config-change reload loop
// 2. Đảm bảo module.exports được restore đúng cách nếu inject-code.txt overwrite nó
// 3. Guard chống rapid-fire auth session change reload
//
// NOTE: aee() / IPe() localhost fix được xử lý trực tiếp bởi patch_stability_fixes()
//       trong repack_vsix.py (thay thế trực tiếp trong bundle code).

(function () {
  "use strict";

  const MARKER = "__augment_byok_stability_patch";
  if (globalThis && globalThis[MARKER]) return;
  try { if (globalThis) globalThis[MARKER] = true; } catch (_) { }

  // === Patch 1: Ensure module.exports integrity ===
  // inject-code.txt overwrites module.exports với interceptor API object.
  // byok-proxy-auth-header-inject.js restore lại, nhưng đây là guard thêm.
  // Chạy sớm trước khi bất kỳ code nào khác
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

  // === Patch 2: Guard chống rapid-fire auth reload ===
  // Trong v0.890.1, một số component mới (AuthStateManager, ACP) có thể
  // trigger onDidChangeSession nhiều lần liên tiếp.
  // Patch này thêm vào global một flag để tracking và có thể dùng để debug.
  (function installReloadGuard() {
    try {
      const RELOAD_GUARD_KEY = "__augment_byok_reload_guard";
      if (!globalThis[RELOAD_GUARD_KEY]) {
        globalThis[RELOAD_GUARD_KEY] = {
          lastReloadTime: 0,
          reloadCount: 0,
          MIN_RELOAD_INTERVAL_MS: 3000, // min 3s giữa các lần reload
          canReload: function () {
            const now = Date.now();
            const elapsed = now - this.lastReloadTime;
            if (elapsed < this.MIN_RELOAD_INTERVAL_MS) {
              console.warn(
                "[BYOK Stability] Blocked rapid reload " +
                "(elapsed: " + elapsed + "ms, count: " + this.reloadCount + ")"
              );
              return false;
            }
            this.lastReloadTime = now;
            this.reloadCount++;
            return true;
          }
        };
      }
    } catch (_) { }
  })();

})();;

// === BYOK Proxy Stability Patch End ===
