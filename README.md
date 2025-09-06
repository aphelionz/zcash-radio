# Zcash Radio

> A community-driven radio station that streams YouTube tracks shared in the Zcash Community Forum.

[![GitHub Pages](https://img.shields.io/badge/view-live_site-brightgreen)](https://aphelionz.github.io/zcash-radio)

---

## Table of Contents

- [Background](#background)
- [Install](#install)
- [Usage](#usage)
- [Maintainers](#maintainers)
- [Contributing](#contributing)
- [License](#license)

---

## Background

The Zcash community has a long-running thread — [*What are you listening to?*](https://forum.zcashcommunity.com/t/what-are-you-listening-to/20456) — where members share music.  

Zcash Radio collects those YouTube links, shuffles them, and streams them in a minimalist fullscreen web player with a “Pennies from Heaven” donation address.

The playlist updates automatically every 6 hours via GitHub Actions.

---

## Install

Clone the repo:

```sh
git clone https://github.com/aphelionz/zcash-radio.git
cd zcash-radio

Build the Rust scanner (requires Rust stable):

cargo run --release --manifest-path zcash-radio-scan/Cargo.toml
```

This produces public/videos.json.

## Usage

### Local server

Run a local web server in the public/ directory:

```bash
cd public
python3 -m http.server 8080
```

Visit http://localhost:8080 and click Start Radio.

### Donation address

The footer displays a fixed donation address defined directly in `public/index.html`.

### Maintainers

@aphelionz

See also AGENTS.md for details on the GitHub Actions agents running this project.

### Contributing

Contributions welcome! Please open issues or PRs.

Guidelines:

* Keep the player lean and dependency-light.
* Use GitHub Actions to extend automation, not third-party CI services.
* Document new behavior in AGENTS.md.

### License

Currently unlicensed. Do whatever you want with it.
