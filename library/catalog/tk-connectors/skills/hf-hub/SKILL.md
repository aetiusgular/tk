---
name: hf-hub
description: Search and inspect Hugging Face Hub models, datasets, Spaces and papers with the Hugging Face connector. Use for model cards, dataset layouts, trending lists and file reads on the Hub.
license: MIT
allowed-tools: hugging-face/hub_repo_search hugging-face/hub_repo_details hugging-face/hf_fs
metadata:
  author: aetiusgular
  version: "0.1.0"
---

Use `hub_repo_search` to find a repository, `hub_repo_details` for its card and files, and `hf_fs` to read one file (`cat hf://models/OWNER/NAME/README.md`) or a listing (`ls hf://models/trending`). Quote parameter counts, licences and context lengths from the model card, not from memory. `dynamic_space` runs a Space, which can cost; ask before running one.
