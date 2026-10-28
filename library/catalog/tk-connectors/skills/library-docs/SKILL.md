---
name: library-docs
description: Look up version-specific documentation and code examples for a library, framework, SDK or CLI with the Context7 connector. Use before writing code against an API whose current shape you are not sure of.
license: MIT
allowed-tools: context7/resolve_library_id context7/query_docs
metadata:
  author: aetiusgular
  version: "0.1.0"
---

First call `resolve-library-id` with the library's name to get its Context7 id. Then call `query-docs` with that id and a question that names the exact thing you need (a function, an option, a migration). Read the version in the answer and say it when you use the result. Do not guess an API from memory when the lookup disagrees with it.
