# ADR-0007 — SQLite como fonte de verdade; `etc/` derivado com blocos gerenciados

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Laragon usa `laragon.ini` + arquivos de config das tools como estado; o usuário edita qualquer coisa e o app regenera partes sem avisar (ex. vhosts `auto.*.conf`). Herd esconde tudo. Precisamos de estado consistente (sites, portas, versões, histórico) **e** de configs editáveis pelo usuário.

## Decisão
`state/forgebox.db` (SQLite) é a fonte de verdade para entidades do domínio. Arquivos em `etc/` são renderizados a partir dela e dos manifestos. Arquivos compartilhados com o usuário (`php.ini`, `nginx.conf`, `my.ini`) usam **blocos gerenciados** (`# forgebox:begin/end`); fora deles o Forgebox nunca escreve. Arquivos 100 % gerados têm cabeçalho explícito. Drift dentro de blocos é detectado e tratado na UI. `etc/sites/*.yaml` é um export legível para git/backup, não a fonte.

## Alternativas rejeitadas
- **Arquivos como fonte de verdade (parse de nginx.conf etc.):** parsers frágeis por servidor; impossível manter invariantes (portas únicas, hosts únicos).
- **Tudo gerado, sem edição do usuário:** inaceitável para o público "controle total".
- **JSON/YAML de estado em vez de SQLite:** sem transações, consultas e histórico.

## Consequências
- Overrides de template em `etc/templates/` para customização estrutural.
- Backup do `.db` antes de cada migration e update.
