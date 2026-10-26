# ADR-0004 — DNS wildcard via resolver próprio + NRPT

**Status:** aceito · **Data:** 2026-10-26

## Contexto
`hosts` do Windows não suporta wildcard. Laragon edita `hosts` por site. Herd embute dnsmasq (`address=/.test/127.0.0.1`) e precisa configurar o sistema para consultá-lo. Acrylic DNS Proxy é a gambiarra comum.

## Decisão
Servidor DNS embutido (hickory) em `127.0.0.1:53` respondendo as zonas configuradas (`.test` por padrão) com `A 127.0.0.1` / `AAAA ::1`. Uma regra NRPT (`Add-DnsClientNrptRule -Namespace ".test" -NameServers 127.0.0.1`) criada pelo helper elevado faz o Windows enviar só `*.test` para nós, mantendo o DNS normal para o resto. Fallback: bloco gerenciado em `hosts`.

## Alternativas rejeitadas
- **Só `hosts`:** sem wildcard, exige admin a cada site/alias.
- **Trocar o DNS do adaptador para 127.0.0.1 + upstream:** frágil com VPN/Wi-Fi; interceptamos todo DNS da máquina.
- **mDNS `.local`:** colide com Bonjour/Windows mDNS; sem subdomínios arbitrários.
- **Acrylic/dnsmasq externos:** dependência extra e configuração global.

## Consequências
- Um UAC único no onboarding.
- Porta 53 pode estar ocupada (Docker, Hyper-V, outro DNS) → usamos `127.0.0.53:53` ou porta alta (NRPT aceita só IP, então preferimos IP alternativo de loopback).
- Diagnóstico de resolução passa a ser parte da UI.
