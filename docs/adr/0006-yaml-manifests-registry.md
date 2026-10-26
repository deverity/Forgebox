# ADR-0006 — Catálogo de tools como manifestos YAML em registry assinado

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Laragon usa `packages.conf` (lista `nome=url`) e lógica por tool dentro do executável. Herd/ServBay têm catálogos fechados. Requisito: descobrir e instalar tools novas sem lançar versão nova do Forgebox, com metadados suficientes para UI, serviço, configuração e updates; permitir que autores distribuam tools "prontas para Forgebox".

## Decisão
Manifesto `forgebox.yaml` (`apiVersion: forgebox.dev/v1`, kinds `Tool | Extension | Stack | Project`) especificado em [04](../04-tool-manifest-spec.md). Registry oficial em git com `index.yaml` assinado (minisign); registries de terceiros; manifesto embutido no zip da tool; manifestos locais do usuário. Lógica por tool fica em manifestos + templates, nunca em código Rust.

## Alternativas rejeitadas
- **TOML/JSON:** YAML tem melhor ergonomia para blocos multi-linha e é o padrão em ecossistemas similares (Homebrew-like, DDEV, GitHub Actions).
- **Scripts (PowerShell/Lua) por tool (modelo scoop/chocolatey):** poder arbitrário = risco; difícil de validar e de gerar UI a partir deles.
- **Catálogo hardcoded:** cada tool nova exige release.

## Consequências
- Schema evolui com `apiVersion`; lint obrigatório no CI do registry.
- Ações de pós-instalação são declarativas e restritas (segurança).
- A UI é gerada a partir do manifesto (quick settings, ações, badges).
