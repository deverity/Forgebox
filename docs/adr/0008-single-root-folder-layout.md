# ADR-0008 — Pasta única (`C:\Forgebox`) e instalação sob demanda

**Status:** aceito · **Data:** 2026-10-26

## Contexto
O público do Forgebox valoriza saber onde cada coisa está (sucesso do Laragon). Herd espalha em `%USERPROFILE%\.config\herd` e embute tudo em um instalador de 490 MB. O usuário pediu compatibilidade "completona" mesmo que precise clicar Instalar.

## Decisão
Raiz única configurável (padrão `C:\Forgebox`) com `bin/ etc/ data/ www/ certs/ logs/ state/ tmp/ backups/ manifests/` ([08](../08-release-layout.md)). Instalador ≤ 25 MB sem tools; catálogo completo exibido; cada tool baixada sob demanda do provider oficial. Modo portátil por `portable.flag`. Modo "por usuário" separa binários do app da raiz de dados, mas a raiz de dados mantém o mesmo layout.

## Alternativas rejeitadas
- **Instalador com stack embutida:** inflado, versões envelhecem no dia seguinte, não escala para o catálogo.
- **Layout em `%APPDATA%`/`%LOCALAPPDATA%`:** caminhos longos, escondidos, ruins para Defender exclusions e para abrir no Explorer.
- **Store do Windows / MSIX:** sandbox impede junctions/PATH/portas com a liberdade necessária.

## Consequências
- Onboarding precisa de internet (ou bundle offline `.fbx`).
- Caminhos curtos e sem espaços por padrão (importante para Composer/npm/MySQL).
