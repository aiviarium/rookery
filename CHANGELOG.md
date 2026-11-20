# Changelog

Notable changes, newest first. A release's notes on GitHub list every pull request.

## Unreleased (0.6.0)

- Plugins have a `plugin.json`. A `rookery.json` is still read, for this release, as a `plugin.json` with host features and provisioners alone. Rookery's own plugins `backup` 0.2.0, `docker` 0.3.0, `github` 0.2.0, `incus` 0.3.0 and `unity` 0.2.0 come with `plugin.json` and still say `rookery ">=0.6"`. A development build of 0.6.0 from before this change can't read them, and stays on the versions it carries.
- The plugin ids `rookery` and `sign-in` are Rookery's own and refused for packages. A package whose id another installed package holds is refused instead of replacing it.
- Host halves of plugins run in an extension host of their own each, on Bun. Its environment is `PATH`, `HOME`, `LANG`, `LC_ALL` and `TZ` from the server's, and `NODE_PATH` and `ROOKERY_EXTHOST_PROTOCOL` set by the server. The server and each host are non-dumpable. A host that holds more memory than `ROOKERY_EXTHOST_MEMORY_MB` (512 MiB) is ended as a crash.
- A method of a host half (`rookery.rpc.handle`) says its level, `own` or `admin`; one that says none is refused.
- `contributes.tools` and a host half's `rookery.agents.tool` are reserved for a later version. A package may declare its tools: Rookery accepts them, drops them when it loads the plugin, and says so at install and in `rookery plugin check`. No MCP server serves them.
