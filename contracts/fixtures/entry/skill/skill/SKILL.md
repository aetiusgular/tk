---
name: web-search
description: Search the web with the Exa connector. Use when a task needs a current fact or a page the model has not seen.
version: "0.0.1"
license: MIT
allowed-tools:
  - exa/web_search_exa
metadata:
  author: aetiusgular
---

Fixture skill for the tk contract tests. Call `web_search_exa` with a short query, read the top results, and cite the page you used.
