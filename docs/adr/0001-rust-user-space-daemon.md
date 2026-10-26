# ADR-0001 — Motor em Rust como daemon user-space

**Status:** aceito · **Data:** 2026-10-26

## Contexto
Laragon (Delphi) roda tudo dentro do processo da GUI: fechar a janela derruba a stack; processos `php-cgi` ficam órfãos. Herd usa Electron + serviço elevado (HerdHelper). XAMPP/WAMP dependem de Windows Services ou do painel aberto.

## Decisão
Um daemon `forgeboxd.exe` em Rust, rodando como o usuário (sem admin, sem Windows Service), instância única, dono de todos os processos via Job Objects, exposto por JSON-RPC em named pipe. GUI e CLI são clientes.

## Alternativas rejeitadas
- **Windows Service para o daemon:** exige admin para instalar, complica portátil, roda em sessão 0 (sem acesso a junctions/perfil do usuário de forma simples).
- **Lógica dentro da GUI (modelo Laragon):** acoplamento, sem CLI real, stack morre com a janela.
- **Go/C#:** Go viável, mas Rust dá controle Win32 sem cgo e binários menores; C# exige runtime ou AOT com limitações.

## Consequências
- Precisamos de um helper elevado mínimo para NRPT/hosts/firewall.
- Toda feature passa por API → paridade CLI/GUI garantida.
- Testabilidade: daemon roda em root temporário em CI.
