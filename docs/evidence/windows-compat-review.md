# 1.8.3 compatibility review

Scope: pending gateway fixes plus Windows portability and release workflow.

Resolved during review:
- Windows native paths, executable discovery, runtime source and browser/process integration replace macOS-only commands.
- Chromium Windows v10 uses AES-256-GCM and a DPAPI-wrapped Local State key; macOS CBC ciphertext is not reused for token-backed imports.
- Failed Windows process inspection blocks writes. Process stop matches the full selected executable path.
- Missing SQLite files are opened read-only without creating empty databases; bundled SQLite removes a Windows CLI prerequisite.
- OS random source replaces timestamp fallback for PKCE. No model requests were made during this compatibility task.
- Queue notices after generated content cannot replay the request.
- CI explicitly selects NSIS on Windows and app/DMG on macOS, tests both native hosts first, and pins release tags to the built commit.

Local verification: 47 Rust tests; Node runtime fixture (old/new loader entry patterns, spaces/Unicode paths, native CLI never executed, sanitized failures); frontend syntax; git diff whitespace checks.

Remaining validation: GitHub Windows runner compilation/DPAPI tests and release builds; actual Windows Qoder login, switching and model calls require a Windows user environment and are not claimed as live-tested.

Primary implementation references:
- [Electron safeStorage](https://www.electronjs.org/docs/latest/api/safe-storage/)
- [Chromium Windows OSCrypt v10](https://raw.githubusercontent.com/chromium/chromium/130.0.6723.58/components/os_crypt/sync/os_crypt_win.cc)

Review outcome: no remaining blocking issue identified in reviewed changes; platform CI must pass before binaries are released.
