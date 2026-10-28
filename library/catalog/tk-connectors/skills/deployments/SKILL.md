---
name: deployments
description: Inspect Vercel projects, deployments, runtime logs and errors, and search Vercel's documentation. Use when a deploy failed, a production error needs a cause, or a Vercel setting is in question.
license: MIT
allowed-tools: vercel/list_projects vercel/list_deployments vercel/get_deployment vercel/get_runtime_logs vercel/get_runtime_errors vercel/search_vercel_documentation
metadata:
  author: aetiusgular
  version: "0.1.0"
---

Start from the project: `list_projects`, then `list_deployments` for the one in question and `get_deployment` for its state and build output. For production errors call `get_runtime_errors` first, then `get_runtime_logs` with the project and time filters. Report what was observed apart from what you suspect. Settings changes (environment variables, domains, routes) are writes on the user's account; confirm before any.
