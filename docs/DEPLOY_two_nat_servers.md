# 部署方案 · 两台异网(不同 NAT)服务器互通（不改代码）

> 场景：两台服务器各处一个内网（不同 NAT）之后，彼此无法直连；目标是让两台上的**客户端相互通信**。约束：**不修改现有代码**，只做安装/部署/配置。
> 日期：2026-09-22 · 关联：`PLAN_D_public_internet.md`（免 VPN 的原生穿透，需改代码，本文不采用）

---

## 1. 关键判断（为什么这样做）

- 现有 `nmd` 用 **`Minimal` 预设**：只做**直连**，**不含** NAT 穿透 / 中继 / 发现。
- 但**联邦机制已实现并测过**（`fed.sync` 目录同步 + `Relay` 跨节点投递，见 `m2b_lan` 集成测试）。
- ⇒ 两台都在 NAT 后时，`nmd` 之间**直接连不上**。**不改代码**的唯一可行路径：**加一层 overlay 虚拟网**让两台 `nmd` 之间"像同网一样能直连"，上层跑**现有 LAN 联邦**即可。
- iroh 原生的免 VPN 穿透（`N0`：中继+打洞+发现）需要 `PLAN_D` 的 D0 代码——按"不动代码"约束，本文**不采用**，仅在 §8 作为未来路径列出。

---

## 2. 拓扑

<p align="center"><img alt="部署拓扑" width="940" style="max-width:100%;height:auto" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA5NDAgNTAwIiB3aWR0aD0iOTQwIiBoZWlnaHQ9IjUwMCI+PGRlZnM+PG1hcmtlciBpZD0iYS1tdXRlZCIgbWFya2VyV2lkdGg9IjEwIiBtYXJrZXJIZWlnaHQ9IjEwIiByZWZYPSI4IiByZWZZPSIzIiBvcmllbnQ9ImF1dG8iIG1hcmtlclVuaXRzPSJzdHJva2VXaWR0aCI+PHBhdGggZD0iTTAsMCBMOCwzIEwwLDYgWiIgZmlsbD0iIzk0YTNiOCIvPjwvbWFya2VyPjxtYXJrZXIgaWQ9ImEtaW5kaWdvIiBtYXJrZXJXaWR0aD0iMTAiIG1hcmtlckhlaWdodD0iMTAiIHJlZlg9IjgiIHJlZlk9IjMiIG9yaWVudD0iYXV0byIgbWFya2VyVW5pdHM9InN0cm9rZVdpZHRoIj48cGF0aCBkPSJNMCwwIEw4LDMgTDAsNiBaIiBmaWxsPSIjNjM2NmYxIi8+PC9tYXJrZXI+PG1hcmtlciBpZD0iYS10ZWFsIiBtYXJrZXJXaWR0aD0iMTAiIG1hcmtlckhlaWdodD0iMTAiIHJlZlg9IjgiIHJlZlk9IjMiIG9yaWVudD0iYXV0byIgbWFya2VyVW5pdHM9InN0cm9rZVdpZHRoIj48cGF0aCBkPSJNMCwwIEw4LDMgTDAsNiBaIiBmaWxsPSIjMGVhNWU5Ii8+PC9tYXJrZXI+PG1hcmtlciBpZD0iYS1ncmVlbiIgbWFya2VyV2lkdGg9IjEwIiBtYXJrZXJIZWlnaHQ9IjEwIiByZWZYPSI4IiByZWZZPSIzIiBvcmllbnQ9ImF1dG8iIG1hcmtlclVuaXRzPSJzdHJva2VXaWR0aCI+PHBhdGggZD0iTTAsMCBMOCwzIEwwLDYgWiIgZmlsbD0iIzEwYjk4MSIvPjwvbWFya2VyPjxtYXJrZXIgaWQ9ImEtYW1iZXIiIG1hcmtlcldpZHRoPSIxMCIgbWFya2VySGVpZ2h0PSIxMCIgcmVmWD0iOCIgcmVmWT0iMyIgb3JpZW50PSJhdXRvIiBtYXJrZXJVbml0cz0ic3Ryb2tlV2lkdGgiPjxwYXRoIGQ9Ik0wLDAgTDgsMyBMMCw2IFoiIGZpbGw9IiNmNTllMGIiLz48L21hcmtlcj48bWFya2VyIGlkPSJhLXNsYXRlIiBtYXJrZXJXaWR0aD0iMTAiIG1hcmtlckhlaWdodD0iMTAiIHJlZlg9IjgiIHJlZlk9IjMiIG9yaWVudD0iYXV0byIgbWFya2VyVW5pdHM9InN0cm9rZVdpZHRoIj48cGF0aCBkPSJNMCwwIEw4LDMgTDAsNiBaIiBmaWxsPSIjNDc1NTY5Ii8+PC9tYXJrZXI+PG1hcmtlciBpZD0iYS12aW9sZXQiIG1hcmtlcldpZHRoPSIxMCIgbWFya2VySGVpZ2h0PSIxMCIgcmVmWD0iOCIgcmVmWT0iMyIgb3JpZW50PSJhdXRvIiBtYXJrZXJVbml0cz0ic3Ryb2tlV2lkdGgiPjxwYXRoIGQ9Ik0wLDAgTDgsMyBMMCw2IFoiIGZpbGw9IiM4YjVjZjYiLz48L21hcmtlcj48L2RlZnM+PHJlY3Qgd2lkdGg9Ijk0MCIgaGVpZ2h0PSI1MDAiIHJ4PSIxOCIgZmlsbD0iI2Y4ZmFmYyIvPjx0ZXh0IHg9IjQwIiB5PSI0MiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjE4LjUiIGZpbGw9IiMxZTI5M2IiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjcwMCI+6YOo572y5ouT5omRIMK3IOS4pOWPsOW8gue9keacjeWKoeWZqOe7jyBvdmVybGF5IOaJk+mAmu+8iOS4jeaUueS7o+egge+8iTwvdGV4dD48dGV4dCB4PSI0MCIgeT0iNjQiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLEJsaW5rTWFjU3lzdGVtRm9udCwnU2Vnb2UgVUknLFJvYm90byxIZWx2ZXRpY2EsQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iIzY0NzQ4YiIgdGV4dC1hbmNob3I9InN0YXJ0IiBmb250LXdlaWdodD0iNDAwIj7lj6rmnInkuKTlj7AgaW1kIOmcgOimgSBvdmVybGF5IOS6kumAmu+8m+WuouaIt+err+i/nuWQhOiHquacrOWcsCBpbWQg5Y2z5Y+v44CC6IGU6YKmKGZlZC5zeW5jK1JlbGF5KeS4uueOsOacieS7o+eggeOAgjwvdGV4dD48cmVjdCB4PSIxMjAiIHk9IjIxNCIgd2lkdGg9IjcwMCIgaGVpZ2h0PSI1MiIgcng9IjEyIiBmaWxsPSIjZWRlOWZlIiBzdHJva2U9IiM4YjVjZjYiIHN0cm9rZS13aWR0aD0iMS44IiBzdHJva2UtZGFzaGFycmF5PSI3IDUiLz48dGV4dCB4PSI0NzAiIHk9IjIzNiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyLjUiIGZpbGw9IiM4YjVjZjYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtd2VpZ2h0PSI3MDAiPk92ZXJsYXkg6Jma5ouf572R77yIVGFpbHNjYWxlIC8gV2lyZUd1YXJk77yJ4oCUIOiHquWKqOepv+mAjyBOQVTvvIznu5nmr4/lj7DnqLPlrpogMTAwLnggSVA8L3RleHQ+PHRleHQgeD0iNDcwIiB5PSIyNTQiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLEJsaW5rTWFjU3lzdGVtRm9udCwnU2Vnb2UgVUknLFJvYm90byxIZWx2ZXRpY2EsQXJpYWwsc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMSIgZmlsbD0iIzY0NzQ4YiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjQwMCI+5Lik5Y+wIGltZCDnu48gb3ZlcmxheSDop4bkuLrigJzlkIznvZHnm7Tov57igJ3vvIzot5HnjrDmnIkgTEFOIOiBlOmCpjwvdGV4dD48cmVjdCB4PSI0MCIgeT0iMzAwIiB3aWR0aD0iNDAwIiBoZWlnaHQ9IjE1MCIgcng9IjE0IiBmaWxsPSIjZmZmIiBzdHJva2U9IiM0NzU1NjkiIHN0cm9rZS13aWR0aD0iMS42IiBzdHJva2UtZGFzaGFycmF5PSI2IDUiLz48dGV4dCB4PSI1NiIgeT0iMzIyIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxCbGlua01hY1N5c3RlbUZvbnQsJ1NlZ29lIFVJJyxSb2JvdG8sSGVsdmV0aWNhLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM0NzU1NjkiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjcwMCI+5YaF572RIEHvvIhOQVQgQe+8iTwvdGV4dD48cmVjdCB4PSI2MCIgeT0iMzM1IiB3aWR0aD0iMTgwIiBoZWlnaHQ9IjYwIiByeD0iMTEiIGZpbGw9IiNmZmZmZmYiIHN0cm9rZT0iIzYzNjZmMSIgc3Ryb2tlLXdpZHRoPSIxLjciLz48cmVjdCB4PSI2MCIgeT0iMzM1IiB3aWR0aD0iMTgwIiBoZWlnaHQ9IjI0IiByeD0iMTEiIGZpbGw9IiNlZWYyZmYiIHN0cm9rZT0iIzYzNjZmMSIgc3Ryb2tlLXdpZHRoPSIwIi8+PHJlY3QgeD0iNjAiIHk9IjM0NyIgd2lkdGg9IjE4MCIgaGVpZ2h0PSIxMiIgcng9IjAiIGZpbGw9IiNlZWYyZmYiIHN0cm9rZT0ibm9uZSIgc3Ryb2tlLXdpZHRoPSIwIi8+PHRleHQgeD0iNzAiIHk9IjM1MiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjM2NmYxIiB0ZXh0LWFuY2hvcj0ic3RhcnQiIGZvbnQtd2VpZ2h0PSI3MDAiPuacjeWKoeWZqEEgwrcgaW1kLUE8L3RleHQ+PHRleHQgeD0iNzAiIHk9IjM3NiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwLjUiIGZpbGw9IiM2NDc0OGIiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjQwMCI+MTAwLngueC54Ojk2MDDvvIhvdmVybGF577yJPC90ZXh0PjxyZWN0IHg9IjI1NSIgeT0iMzM1IiB3aWR0aD0iMTcwIiBoZWlnaHQ9IjYwIiByeD0iMTEiIGZpbGw9IiNmZmZmZmYiIHN0cm9rZT0iIzBlYTVlOSIgc3Ryb2tlLXdpZHRoPSIxLjciLz48cmVjdCB4PSIyNTUiIHk9IjMzNSIgd2lkdGg9IjE3MCIgaGVpZ2h0PSIyNCIgcng9IjExIiBmaWxsPSIjZTBmMmZlIiBzdHJva2U9IiMwZWE1ZTkiIHN0cm9rZS13aWR0aD0iMCIvPjxyZWN0IHg9IjI1NSIgeT0iMzQ3IiB3aWR0aD0iMTcwIiBoZWlnaHQ9IjEyIiByeD0iMCIgZmlsbD0iI2UwZjJmZSIgc3Ryb2tlPSJub25lIiBzdHJva2Utd2lkdGg9IjAiLz48dGV4dCB4PSIyNjUiIHk9IjM1MiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjMGVhNWU5IiB0ZXh0LWFuY2hvcj0ic3RhcnQiIGZvbnQtd2VpZ2h0PSI3MDAiPuacrOWcsOWuouaIt+errzwvdGV4dD48dGV4dCB4PSIyNjUiIHk9IjM3NiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwLjUiIGZpbGw9IiM2NDc0OGIiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjQwMCI+5Lq6L0FnZW50L2VjaG/igKY8L3RleHQ+PGxpbmUgeDE9IjI0MCIgeTE9IjM2NSIgeDI9IjI1NSIgeTI9IjM2NSIgc3Ryb2tlPSIjOTRhM2I4IiBzdHJva2Utd2lkdGg9IjEuNCIgbWFya2VyLWVuZD0idXJsKCNhLW11dGVkKSIvPjxyZWN0IHg9IjUwMCIgeT0iMzAwIiB3aWR0aD0iNDAwIiBoZWlnaHQ9IjE1MCIgcng9IjE0IiBmaWxsPSIjZmZmIiBzdHJva2U9IiM0NzU1NjkiIHN0cm9rZS13aWR0aD0iMS42IiBzdHJva2UtZGFzaGFycmF5PSI2IDUiLz48dGV4dCB4PSI1MTYiIHk9IjMyMiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNDc1NTY5IiB0ZXh0LWFuY2hvcj0ic3RhcnQiIGZvbnQtd2VpZ2h0PSI3MDAiPuWGhee9kSBC77yITkFUIELvvIk8L3RleHQ+PHJlY3QgeD0iNzAwIiB5PSIzMzUiIHdpZHRoPSIxODAiIGhlaWdodD0iNjAiIHJ4PSIxMSIgZmlsbD0iI2ZmZmZmZiIgc3Ryb2tlPSIjNjM2NmYxIiBzdHJva2Utd2lkdGg9IjEuNyIvPjxyZWN0IHg9IjcwMCIgeT0iMzM1IiB3aWR0aD0iMTgwIiBoZWlnaHQ9IjI0IiByeD0iMTEiIGZpbGw9IiNlZWYyZmYiIHN0cm9rZT0iIzYzNjZmMSIgc3Ryb2tlLXdpZHRoPSIwIi8+PHJlY3QgeD0iNzAwIiB5PSIzNDciIHdpZHRoPSIxODAiIGhlaWdodD0iMTIiIHJ4PSIwIiBmaWxsPSIjZWVmMmZmIiBzdHJva2U9Im5vbmUiIHN0cm9rZS13aWR0aD0iMCIvPjx0ZXh0IHg9IjcxMCIgeT0iMzUyIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxCbGlua01hY1N5c3RlbUZvbnQsJ1NlZ29lIFVJJyxSb2JvdG8sSGVsdmV0aWNhLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM2MzY2ZjEiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjcwMCI+5pyN5Yqh5ZmoQiDCtyBpbWQtQjwvdGV4dD48dGV4dCB4PSI3MTAiIHk9IjM3NiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwLjUiIGZpbGw9IiM2NDc0OGIiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjQwMCI+MTAwLngueC54Ojk2MDDvvIhvdmVybGF577yJPC90ZXh0PjxyZWN0IHg9IjUxNSIgeT0iMzM1IiB3aWR0aD0iMTcwIiBoZWlnaHQ9IjYwIiByeD0iMTEiIGZpbGw9IiNmZmZmZmYiIHN0cm9rZT0iIzBlYTVlOSIgc3Ryb2tlLXdpZHRoPSIxLjciLz48cmVjdCB4PSI1MTUiIHk9IjMzNSIgd2lkdGg9IjE3MCIgaGVpZ2h0PSIyNCIgcng9IjExIiBmaWxsPSIjZTBmMmZlIiBzdHJva2U9IiMwZWE1ZTkiIHN0cm9rZS13aWR0aD0iMCIvPjxyZWN0IHg9IjUxNSIgeT0iMzQ3IiB3aWR0aD0iMTcwIiBoZWlnaHQ9IjEyIiByeD0iMCIgZmlsbD0iI2UwZjJmZSIgc3Ryb2tlPSJub25lIiBzdHJva2Utd2lkdGg9IjAiLz48dGV4dCB4PSI1MjUiIHk9IjM1MiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjMGVhNWU5IiB0ZXh0LWFuY2hvcj0ic3RhcnQiIGZvbnQtd2VpZ2h0PSI3MDAiPuacrOWcsOWuouaIt+errzwvdGV4dD48dGV4dCB4PSI1MjUiIHk9IjM3NiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sQmxpbmtNYWNTeXN0ZW1Gb250LCdTZWdvZSBVSScsUm9ib3RvLEhlbHZldGljYSxBcmlhbCxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEwLjUiIGZpbGw9IiM2NDc0OGIiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjQwMCI+5Lq6L0FnZW50L2VjaG/igKY8L3RleHQ+PGxpbmUgeDE9IjcwMCIgeTE9IjM2NSIgeDI9IjY4NSIgeTI9IjM2NSIgc3Ryb2tlPSIjOTRhM2I4IiBzdHJva2Utd2lkdGg9IjEuNCIgbWFya2VyLWVuZD0idXJsKCNhLW11dGVkKSIvPjxsaW5lIHgxPSIxNTAiIHkxPSIzMzUiIHgyPSIzMDAiIHkyPSIyNjYiIHN0cm9rZT0iIzhiNWNmNiIgc3Ryb2tlLXdpZHRoPSIyIiBtYXJrZXItZW5kPSJ1cmwoI2EtdmlvbGV0KSIvPjxsaW5lIHgxPSI3OTAiIHkxPSIzMzUiIHgyPSI2NDAiIHkyPSIyNjYiIHN0cm9rZT0iIzhiNWNmNiIgc3Ryb2tlLXdpZHRoPSIyIiBtYXJrZXItZW5kPSJ1cmwoI2EtdmlvbGV0KSIvPjxsaW5lIHgxPSIyNDAiIHkxPSIyNDAiIHgyPSI3MDAiIHkyPSIyNDAiIHN0cm9rZT0iIzEwYjk4MSIgc3Ryb2tlLXdpZHRoPSIyLjQiIHN0cm9rZS1kYXNoYXJyYXk9IjYgNCIvPjx0ZXh0IHg9IjQ3MCIgeT0iMTk2IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxCbGlua01hY1N5c3RlbUZvbnQsJ1NlZ29lIFVJJyxSb2JvdG8sSGVsdmV0aWNhLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTEuNSIgZmlsbD0iIzEwYjk4MSIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC13ZWlnaHQ9IjcwMCI+aW1kLUEg4oeEIGltZC1C77ya55uu5b2V5ZCM5q2lICsg6Leo5pyN5Yqh5ZmoIFJlbGF5IOaKlemAkjwvdGV4dD48dGV4dCB4PSI0MCIgeT0iNDc4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxCbGlua01hY1N5c3RlbUZvbnQsJ1NlZ29lIFVJJyxSb2JvdG8sSGVsdmV0aWNhLEFyaWFsLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM2NDc0OGIiIHRleHQtYW5jaG9yPSJzdGFydCIgZm9udC13ZWlnaHQ9IjYwMCI+5a6i5oi356uvIEEg4oaSIOWuouaIt+erryBC77ya6L+e5pys5ZywIGltZC1BIOKGkiBpbWQtQSDnu48gb3ZlcmxheSDogZTpgqbliLAgaW1kLUIg4oaSIGltZC1CIOacrOWcsOaKlemAkuOAgjwvdGV4dD48L3N2Zz4="/></p>

**要点**：只有**两台 nmd** 需要经 overlay 互通；**客户端连各自本地 nmd**（本机/本内网，直连即可）。跨内网这一跳只发生在 `nmd-A ⇄ nmd-B` 之间，由 overlay 负责穿透。

---

## 3. 连通性底座三选一

| 方案 | 原理 | 需路由器权限 | 改代码 | 推荐 |
|---|---|---|---|---|
| **A. Tailscale**（托管协调面） | 自动 NAT 打洞 + DERP 中继兜底，给每台稳定 `100.x` IP | 否（自己穿透） | 否 | ★★★ 最省事 |
| **B. WireGuard / ZeroTier / headscale** | 自建 overlay | WireGuard 需一端有公网入口 | 否 | ★★ 自主可控 |
| **C. 端口转发** | 一侧路由器把 UDP 端口转发到该侧 nmd，另一侧直连它 | 是（一侧） | 否 | ★ 有条件时 |
| D. iroh 原生 N0（中继+打洞+发现） | nmspace 自己穿透，免 overlay | 否 | **是**（PLAN_D D0） | 未来 |

**本文主推 A（Tailscale）**：零路由器权限、零代码、给两台稳定 IP，`nmd` 直接把它当"同网"。§6/§7 给 B、C 的要点。

---

## 4. 详细部署（方案 A：Tailscale + 现有 nmd 联邦）

### 4.0 前置
- 两台服务器（Linux/macOS 均可），能访问外网；各自能本地编译或拷入 `nmd` 二进制。
- 一个 Tailscale 账号（免费档即可）。

### 4.1 装 overlay（两台都做）
```bash
# Linux
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up            # 浏览器登录同一账号(同一 tailnet)
tailscale ip -4              # 记下本机 overlay IP，形如 100.x.y.z
# macOS：装 Tailscale App 登录，或 brew install tailscale
```
**连通性自检**（在 A 上 ping B 的 100.x）：
```bash
ping 100.<B 的 overlay IP>
```
能通即代表 overlay 已打通（Tailscale 已完成 NAT 穿透）。

### 4.2 放置 nmspace（两台都做）
```bash
# 在项目里编译 release 版
cargo build --release -p nmd
# 得到 target/release/nmd（或 ~/.cargo-shared-target/release/nmd）。拷到各服务器工作目录。
# 客户端（可选，用于验证）：cargo build --release -p nm-echo
```

### 4.3 配置并首启（拿地址）
两台各建 `nmd.toml`（**先不填 peers**），固定端口便于稳定：
```toml
# nmd.toml（A、B 各一份，identity 各自生成、勿共用）
identity  = "nmd.identity"
db        = "nmd.redb"
bind_port = 9600
```
各跑一次拿地址：
```bash
./nmd                        # 或 target/release/nmd
# 输出：NM_NODE_ADDR={"id":"<A的公钥>","addrs":[{"Ip":"100.x.x.x:9600"},{"Ip":"192.168.x.x:9600"},...]}
```
> 地址里会同时列出 `100.x`(overlay) 和内网 `192.168.x`——**照抄整段 JSON 即可**，iroh 会自动挑能连通的那条（跨内网时 `100.x` 通、`192.168.x` 不通，iroh 用前者）。记下各自这段，然后 `Ctrl-C` 停掉。

### 4.4 互填对方地址、重启
把**对方的 `NM_NODE_ADDR`**填进各自 `nmd.toml`：
```toml
# A 的 nmd.toml 追加：
[[peers]]
addr = '<B 打印的 NM_NODE_ADDR JSON>'
```
```toml
# B 的 nmd.toml 追加：
[[peers]]
addr = '<A 打印的 NM_NODE_ADDR JSON>'
```
两台重启 `nmd`，应看到日志：
```
node serving ...
federation sync started   peers=1
```
> 因 `identity` 与 `bind_port` 固定，两台地址**跨重启稳定**，peer 配置**长期有效**（除非换 overlay IP）。建议用 systemd/pm2 常驻。

### 4.5 客户端接入、跨服务器验证
- **本地客户端连本地 nmd**：在 A 上把 `nm-echo`/App 的 `--node` 设为 **nmd-A 的地址**（本机，用 `127.0.0.1:9600` 或 nmd-A 打印的地址均可）。B 侧同理连 nmd-B。
- **验证跨服务器**：在 **B** 上跑一个回声机器人（注册到 nmd-B）：
  ```bash
  ./nm-echo --node '<nmd-B 的 NM_NODE_ADDR>' --name "B上的机器人"
  # 记下它打印的 id
  ```
  在 **A** 上用客户端（App / 一个发消息的 nm-client）连 nmd-A，**目录刷新**应能看到"B上的机器人"（经联邦同步而来），向它发消息 → 应收到回声。
  这条链路即：`客户端A → nmd-A →(overlay)→ nmd-B → 机器人`，往返成功即代表两台异网服务器互通达成。

### 4.6 常驻（systemd 示例，两台各配）
```ini
# /etc/systemd/system/nmd.service
[Unit]
Description=nmspace node
After=network-online.target tailscaled.service
[Service]
WorkingDirectory=/opt/nmspace
ExecStart=/opt/nmspace/nmd
Restart=always
Environment=RUST_LOG=nmd=info,nm_node=info
[Install]
WantedBy=multi-user.target
```
```bash
sudo systemctl enable --now nmd
```

---

## 5. 验证清单

- [ ] `tailscale ip -4` 两台各有 `100.x`；互 ping 通。
- [ ] 各 `nmd` 日志：`node serving` + `federation sync started peers=1`。
- [ ] A 侧目录查询能看到 B 上注册的实体（联邦同步生效）。
- [ ] A 侧客户端向 B 侧实体发消息 → 收到回复（跨服务器 Relay 投递生效）。
- [ ] 重启任一 `nmd`，地址不变、peer 配置仍有效（identity+固定端口）。

---

## 6. 备选 B：WireGuard（自建 overlay，不依赖 Tailscale 托管面）
- 一端（有公网入口的那台，或一台云跳板）作 WG 服务端，另一端作 client；建 `10.x` overlay。
- 打通后与 §4.3 起完全相同：`nmd` 用各自 `10.x:9600`，互填 `NM_NODE_ADDR`。
- 取舍：更自主，但要自己管密钥/端点/保活；纯双 NAT（两端都无公网）时 WG 需一台有公网的中转。

## 7. 备选 C：端口转发（无 overlay，一侧可控路由器）
- 在**一侧**路由器把 `UDP 9600` 转发到该侧 nmd 主机，使其获得"公网可达 `公网IP:9600`"。
- 该侧 `nmd` 的 peer 地址用 `公网IP:9600`；另一侧直连它即可（`Minimal` 直连只需**一端可达**）。
- 取舍：省一层 VPN，但需路由器权限 + 该侧最好静态公网 IP；双方都不可达时不适用。

---

## 8. 安全与运维

- **overlay 层**：用 Tailscale ACL / WireGuard 对端白名单，把 `nmd` 的 `9600` 只暴露给 overlay 网段，不要对公网裸开。
- **nmspace 层**：命令类调用已有 `Grant` 授权；**联邦白名单**（只认已配置对等公钥的 `fed.sync`/`Relay`）是 `PLAN_D` 的加固项，本"不改代码"方案下**暂缺**——所以务必靠 overlay ACL 把网络面收好。
- **私钥**：`nmd.identity` 是节点私钥，**每台一份、勿共用、勿入库**（已加 `.gitignore`）。
- **常驻**：systemd 拉起，`tailscaled` 先起；`RUST_LOG=nm_node=info` 观察 `session up`/`recv gram`/联邦同步。

---

## 9. 取舍与未来

| | 本方案（overlay + 现有联邦） | 未来（PLAN_D · iroh 原生 N0） |
|---|---|---|
| 改代码 | 否 | 是（D0 ≈ 120–200 行） |
| 依赖 | 依赖 overlay（Tailscale/WG） | 依赖 relay/dns（可用 n0 或自建） |
| NAT 穿透 | 由 overlay 负责 | 由 iroh 负责（打洞+中继） |
| 寻址 | 手工互填 `NM_NODE_ADDR`（固定端口稳定） | 按公钥自动发现，免手填 |
| 适用 | **现在就能上，两台/少量节点最省事** | 节点多、免 VPN、要自动发现时更优 |

**结论**：你现在两台异网服务器，**不动代码**的最快方案就是 **Tailscale + 现有 nmd 联邦**（§4）——十几分钟可通。等将来要免 VPN / 多节点自动组网，再按 `PLAN_D` 做 iroh 原生 N0（那需要改代码）。

---

*本方案只做"连通性底座 + 部署配置"，nmspace 侧零改动：现有 `Minimal` 直连 + 已实现的联邦(fed.sync/Relay) 在 overlay 之上原样工作。*
