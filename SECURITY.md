# Security policy

## Supported versions

Only the latest 0.6.x release gets security fixes.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: open the repository's
[Security tab](https://github.com/zeo/capscr/security/advisories/new) and pick
"Report a vulnerability". Please don't file a public issue for anything
exploitable.

Include the capscr version, your OS, and the steps or a sample file that
shows the problem.

## Scope

These are the boundaries capscr relies on, and the most useful places to
look:

- the plugin sandbox: wasm capability gates, fuel and epoch limits, the
  `fetch` URL patterns
- marketplace registry verification: the minisign signature check and
  `min_capscr_version` enforcement
- the SSRF guard on upload destinations and plugin `fetch`
- the scope of the asset protocol the webview reads captures through
- SFTP host-key trust: a new key needs the user's OK before the first upload
