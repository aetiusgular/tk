---
name: web-search
description: Search the web with the Exa connector. Use when a task needs a current fact, a page the model has not seen, or sources to cite.
license: MIT
allowed-tools: exa/web_search_exa exa/web_fetch_exa
metadata:
  author: aetiusgular
  version: "0.1.0"
---

Call `web_search_exa` with a short query (3 to 8 words). Read the top results; call `web_fetch_exa` on the one page that answers the question before citing it. Cite the page you used by title and URL. Prefer the original source (the project's own docs, the paper, the vendor page) over an aggregator. One search is usually enough; do not repeat a query with the same words.
