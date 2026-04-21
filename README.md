# ccl-server

`ccl-server` is a single-binary private server for [Friday the 13th: The Game](https://www.playstation.com/en-us/games/friday-the-13th-the-game) on PlayStation. Download one file, run it, host your own camp.

[![License](https://img.shields.io/github/license/CampCrystalLake/ccl-server)](LICENSE)
[![Wiki](https://img.shields.io/badge/wiki-campcrystallake.xyz-c70039)](https://campcrystallake.xyz)
[![Rust Edition](https://img.shields.io/badge/rust-2024-ed7a1f)](https://www.rust-lang.org/)
[![Release](https://github.com/CampCrystalLake/ccl-server/actions/workflows/release.yml/badge.svg)](https://github.com/CampCrystalLake/ccl-server/actions/workflows/release.yml)

Licensed under MIT.

## About

Friday the 13th: The Game shut down its official servers in 2024, taking the game offline on every platform. Other community servers exist on PC, but for PlayStation there wasn't one. The [Camp Crystal Lake](https://discord.gg/dn4pacGHe3) server went live on January 25, 2026 as the first proper PlayStation revival, standing on top of years of community protocol work.

`ccl-server` is the standalone counterpart -- a lighter version anyone can run at home for their own friends.

## Status

Early scaffolding. First release targeted for **July 2026**. In the meantime, play on the main [Camp Crystal Lake](https://discord.gg/dn4pacGHe3) community server.

## Install

Once released, grab a prebuilt binary or installer from the [Releases](https://github.com/CampCrystalLake/ccl-server/releases) page. Linux and Windows builds are published for every tagged release with SHA256 checksums and VirusTotal scans in the release notes.

```sh
# Linux / one-liner
curl -LsSf https://github.com/CampCrystalLake/ccl-server/releases/latest/download/ccl-server-installer.sh | sh

# Windows (PowerShell)
powershell -c "irm https://github.com/CampCrystalLake/ccl-server/releases/latest/download/ccl-server-installer.ps1 | iex"

# Windows MSI
# grab ccl-server-*.msi from the Releases page
```

## Usage

Full walkthrough (PS4/PS5 DNS setup, port forwarding, the works) lives on the wiki: <https://campcrystallake.xyz/guides/hosting/getting-started/>.

## License

MIT. See [LICENSE](LICENSE).

<div align="center">
  Made with ❤️ at Camp Crystal Lake
</div>
