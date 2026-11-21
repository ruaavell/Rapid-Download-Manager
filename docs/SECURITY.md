# Security model

- Accept only `http` and `https` URLs.
- Do not execute downloaded files.
- Do not accept arbitrary shell commands from browser messages.
- Native Messaging host validates message size and URL scheme.
- No TCP listener is exposed.
- Browser extension sends link URL and referrer only in this release.
- Cookies, passwords, and authorization headers are intentionally not collected.
- The app stores task URLs locally in SQLite; URLs may contain sensitive query tokens.
  Users should protect their local account and database accordingly.
