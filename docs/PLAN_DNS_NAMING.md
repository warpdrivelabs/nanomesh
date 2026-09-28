# Nano Mesh 去中心命名系统实现方案

> 在现有对等节点 `nmd` 上增加一套类似 DNS 的分布式名字。
> 域名 `mesh.example` 指向一台节点的公钥；用户名 `jeff@mesh.example` 指向一个用户的公钥，且 `mesh.example` 就是该用户的家节点。
> 名字去中心复制，胜者由确定的比较规则算出，愈合后不会留下两个有效绑定。
> 本文是实现方案。现有拨号、目录、gossip 和离线库保持不动，名字只是它们前面的一层别名。
> 先前的 [`DECENTRALIZED_NAMING_DESIGN.md`](./DECENTRALIZED_NAMING_DESIGN.md) 讨论的是 `name.zone` 与本地备注。本文的产品形态以域名和 `local@domain` 为准。

## 1. 要做成什么样

客户端以后可以不粘贴 64 位 hex：

- 连接一台节点服务时填写 `mesh.example`，系统把它解析成那台 `nmd` 的公钥，再走现有的 `connect`。
- 给某人发消息时填写 `jeff@mesh.example`，系统把它解析成 Jeff 的用户公钥，再走现有的 `send_to`。家节点就是 `mesh.example` 对应的那台 `nmd`。
- 一台节点可以同时拥有 `mesh.example` 和 `lab.example`。两个名字都指向同一把节点公钥。
- 一个域名不能同时指向两把节点公钥。一个 `local@domain` 不能同时指向两个用户。

<p align="center"><img alt="两层名字" width="960" style="max-width:100%;height:auto" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA5NjAgNTIwIiB3aWR0aD0iOTYwIiBoZWlnaHQ9IjUyMCI+CjxyZWN0IHdpZHRoPSI5NjAiIGhlaWdodD0iNTIwIiByeD0iMTYiIGZpbGw9IiNmOGZhZmMiLz4KPHRleHQgeD0iMzYiIHk9IjQwIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjE4IiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGYxNzJhIj7lm74gMSDCtyDkuKTlsYLlkI3lrZfvvJrln5/lkI3mjIflkJHoioLngrnvvIzpgq7nrrHmjIflkJHnlKjmiLc8L3RleHQ+Cjx0ZXh0IHg9IjM2IiB5PSI2NCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iIzY0NzQ4YiI+5LiA5Liq6IqC54K55YWs6ZKl5Y+v5Lul5oyC5aSa5Liq5Z+f5ZCN77yb5LiA5Liq5Z+f5ZCN5Y+q6IO95oyH5ZCR5LiA5Liq6IqC54K55YWs6ZKl44CC55So5oi35ZCN55qE5a626IqC54K55bCx5piv6K+l5Z+f5ZCN55qE6IqC54K544CCPC90ZXh0Pgo8cmVjdCB4PSIzNiIgeT0iODgiIHdpZHRoPSI0MjAiIGhlaWdodD0iMTgwIiByeD0iMTIiIGZpbGw9IiNmZmYiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjYiLz4KPHRleHQgeD0iNTIiIHk9IjExNiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxNCIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzBmNzY2ZSI+6IqC54K55Z+f5ZCNIE5vZGVOYW1lPC90ZXh0Pgo8dGV4dCB4PSI1MiIgeT0iMTQ2IiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLG1vbm9zcGFjZSIgZm9udC1zaXplPSIxMyIgZmlsbD0iIzBmMTcyYSI+bWVzaC5leGFtcGxlPC90ZXh0Pgo8dGV4dCB4PSI1MiIgeT0iMTY4IiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLG1vbm9zcGFjZSIgZm9udC1zaXplPSIxMyIgZmlsbD0iIzBmMTcyYSI+bGFiLmV4YW1wbGU8L3RleHQ+Cjx0ZXh0IHg9IjI1MCIgeT0iMTU3IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmaWxsPSIjMWZiMTgyIj7ihpIg5ZCM5LiA5oqK6IqC54K55YWs6ZKlPC90ZXh0Pgo8dGV4dCB4PSI1MiIgeT0iMjA2IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjQ3NDhiIj5ubWQg55qEIEVuZHBvaW50SWTvvIgzMiDlrZfoioIgRWQyNTUxOe+8iTwvdGV4dD4KPHRleHQgeD0iNTIiIHk9IjIyOCIgZm9udC1mYW1pbHk9InVpLW1vbm9zcGFjZSxNZW5sbyxtb25vc3BhY2UiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM0NzU1NjkiPmE3ODAwMGNj4oCmYWFhODwvdGV4dD4KPHRleHQgeD0iNTIiIHk9IjI1MCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iIzY0NzQ4YiI+5ouo5Y+35LuN6LWw546w5pyJIHBrYXJyIC8g5Lit57un77yM5ZCN5a2X5Y+q5piv6L+Z5oqK5YWs6ZKl55qE5Yir5ZCN44CCPC90ZXh0Pgo8cmVjdCB4PSI1MDAiIHk9Ijg4IiB3aWR0aD0iNDI0IiBoZWlnaHQ9IjE4MCIgcng9IjEyIiBmaWxsPSIjZmZmIiBzdHJva2U9IiM2MzY2ZjEiIHN0cm9rZS13aWR0aD0iMS42Ii8+Cjx0ZXh0IHg9IjUxNiIgeT0iMTE2IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjE0IiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjNDMzOGNhIj7nlKjmiLflkI0gVXNlck5hbWU8L3RleHQ+Cjx0ZXh0IHg9IjUxNiIgeT0iMTUwIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLG1vbm9zcGFjZSIgZm9udC1zaXplPSIxNCIgZmlsbD0iIzBmMTcyYSI+amVmZkBtZXNoLmV4YW1wbGU8L3RleHQ+Cjx0ZXh0IHg9IjUxNiIgeT0iMTc2IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjQ3NDhiIj5sb2NhbCA9IGplZmbjgIDjgIBkb21haW4gPSBtZXNoLmV4YW1wbGU8L3RleHQ+Cjx0ZXh0IHg9IjUxNiIgeT0iMjA2IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjQ3NDhiIj7nlKjmiLflhazpkqXvvIhFbnRpdHlJZO+8iTwvdGV4dD4KPHRleHQgeD0iNTE2IiB5PSIyMjgiIGZvbnQtZmFtaWx5PSJ1aS1tb25vc3BhY2UsTWVubG8sbW9ub3NwYWNlIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNDc1NTY5Ij44OWUxODJhOOKApjwvdGV4dD4KPHRleHQgeD0iNTE2IiB5PSIyNTAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM2NDc0OGIiPmhvbWUgbm9kZSA9IG1lc2guZXhhbXBsZSDop6Plh7rnmoTpgqPmioroioLngrnlhazpkqU8L3RleHQ+CjxyZWN0IHg9IjM2IiB5PSIyOTIiIHdpZHRoPSI4ODgiIGhlaWdodD0iMTg4IiByeD0iMTIiIGZpbGw9IiNmZmYiIHN0cm9rZT0iI2NiZDVlMSIgc3Ryb2tlLXdpZHRoPSIxLjIiLz4KPHRleHQgeD0iNTIiIHk9IjMyMiIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxNCIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzBmMTcyYSI+5LiN5Y+Y6YePPC90ZXh0Pgo8dGV4dCB4PSI1MiIgeT0iMzUwIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmaWxsPSIjMzM0MTU1Ij5EMSAg5b2S5LiA5YyW5ZCO55qE5Z+f5ZCN5YWo572R6Iez5aSa5LiA5p2h5pyJ5pWI6K6w5b2V44CC5ZCO5p2l6ICF5LiN6IO955So5pu05aSn55qE5pe26Ze05oiz5oqK5ZCN5a2X5pS55YaZ5Yiw5Y+m5LiA5oqK6ZKl5YyZ5LiK44CCPC90ZXh0Pgo8dGV4dCB4PSI1MiIgeT0iMzc2IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmaWxsPSIjMzM0MTU1Ij5EMiAg5ZCM5LiA6IqC54K55YWs6ZKl5Y+v5Lul5ZCM5pe25oul5pyJIG1lc2guZXhhbXBsZSDkuI4gbGFiLmV4YW1wbGXjgILov5nmmK/kuIDlr7nlpJrvvIzkuI3mmK/lpJrlr7nkuIDjgII8L3RleHQ+Cjx0ZXh0IHg9IjUyIiB5PSI0MDIiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZpbGw9IiMzMzQxNTUiPlUxICBqZWZmQG1lc2guZXhhbXBsZSDlnKjor6Xln5/lkI3kuIvllK/kuIDjgILlj6rmnInlvZPliY3mi6XmnIkgbWVzaC5leGFtcGxlIOeahOiKgueCueWPr+S7peS4uuWug+etvuWPkeeUqOaIt+WQjeOAgjwvdGV4dD4KPHRleHQgeD0iNTIiIHk9IjQyOCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMyIgZmlsbD0iIzMzNDE1NSI+VTIgIOWQjOS4gOS4queUqOaIt+WFrOmSpeWPr+S7peacieWkmuS4quWIq+WQje+8iGplZmZAbWVzaC5leGFtcGxlIOS4jiBqQGxhYi5leGFtcGxl77yJ77yM5q+P5Liq5Yir5ZCN5LuN55Sx5ZCE6Ieq55qE5a626IqC54K56IOM5Lmm44CCPC90ZXh0Pgo8dGV4dCB4PSI1MiIgeT0iNDU4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjQ3NDhiIj7lhazpkqXmnKzouqvku43nhLbmmK/kuovlrp7ln7rlh4bjgILlkI3lrZfop6PmnpDlpLHotKXml7bvvIzosIPnlKjmlrnlj6/ku6XpgIDlm54gNjQg5L2NIGhleO+8jOeOsOacieaLqOWPt+S4juWPkeS/oei3r+W+hOS4jeWPmOOAgjwvdGV4dD4KPC9zdmc+"/></p>

原始公钥仍然是可验证的事实。名字旁边必须能展开看到公钥。解析失败时，界面允许退回粘贴 hex，旧客户端不受影响。

## 2. 为什么不能靠「后写覆盖」

目录里现有的 LWW 是「`updated_at` 更大者胜」。若名字也这么做，后声明的人可以用一个更大的时间戳把 `mesh.example` 改绑到自己的钥匙上。这会制造冲突，也会让抢注变成覆盖。

命名系统不使用这种 LWW。一条已经生效的绑定只接受两种变更：

- **原钥匙自己续期、改展示信息、主动释放。** 目标公钥不变。
- **转移。** 旧钥匙和新钥匙对同一条转移记录各自签名，原子替换。

别人用更晚的时间戳声明同一个名字，结果是 `NAME_TAKEN`，不会改写胜者。

分区时两边可能短时间各接受一个声明。愈合之后，所有节点用同一套比较键重新计算，只保留一个胜者，另一个变成拒绝记录。短暂分歧可以发生，持久的双绑定不可以留下。

<p align="center"><img alt="域名抢注" width="960" style="max-width:100%;height:auto" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA5NjAgNDYwIiB3aWR0aD0iOTYwIiBoZWlnaHQ9IjQ2MCI+CjxyZWN0IHdpZHRoPSI5NjAiIGhlaWdodD0iNDYwIiByeD0iMTYiIGZpbGw9IiNmOGZhZmMiLz4KPHRleHQgeD0iMzYiIHk9IjQwIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjE4IiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGYxNzJhIj7lm74gMiDCtyDln5/lkI3miqLms6jvvJrlhYjliLDlhYjlvpfvvIznpoHmraLlkI7mnaXogIXopobnm5Y8L3RleHQ+Cjx0ZXh0IHg9IjM2IiB5PSI2NCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iIzY0NzQ4YiI+5q+U6L6D6ZSu5pivIChjbGFpbV90cywgbm9kZV9pZCnvvIzlsI/ogIXog5zjgILlkIzkuIDmiorpkqXljJnku6XlkI7lj6rog73nu63mnJ/vvIzkuI3og73miorlkI3lrZfmlLnnu5HliLDliKvkurrjgII8L3RleHQ+CjxyZWN0IHg9IjQ4IiB5PSI5NiIgd2lkdGg9IjIwMCIgaGVpZ2h0PSI2NCIgcng9IjEwIiBmaWxsPSIjZmZmIiBzdHJva2U9IiMxZmIxODIiIHN0cm9rZS13aWR0aD0iMS42Ii8+Cjx0ZXh0IHg9IjE0OCIgeT0iMTI0IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGY3NjZlIj7oioLngrkgQSDlo7DmmI48L3RleHQ+Cjx0ZXh0IHg9IjE0OCIgeT0iMTQ0IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLG1vbm9zcGFjZSIgZm9udC1zaXplPSIxMSIgZmlsbD0iIzY0NzQ4YiI+dCA9IDEwMDwvdGV4dD4KPHJlY3QgeD0iMzgwIiB5PSI5NiIgd2lkdGg9IjIwMCIgaGVpZ2h0PSI2NCIgcng9IjEwIiBmaWxsPSIjZmZmIiBzdHJva2U9IiM2MzY2ZjEiIHN0cm9rZS13aWR0aD0iMS42Ii8+Cjx0ZXh0IHg9IjQ4MCIgeT0iMTI0IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjNDMzOGNhIj7lkb3lkI3popHpgZM8L3RleHQ+Cjx0ZXh0IHg9IjQ4MCIgeT0iMTQ0IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLG1vbm9zcGFjZSIgZm9udC1zaXplPSIxMSIgZmlsbD0iIzY0NzQ4YiI+bm1zcGFjZS1uYW1lczwvdGV4dD4KPHJlY3QgeD0iNzEyIiB5PSI5NiIgd2lkdGg9IjIwMCIgaGVpZ2h0PSI2NCIgcng9IjEwIiBmaWxsPSIjZmZmIiBzdHJva2U9IiNlMTFkNDgiIHN0cm9rZS13aWR0aD0iMS42Ii8+Cjx0ZXh0IHg9IjgxMiIgeT0iMTI0IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjYmUxMjNjIj7oioLngrkgQiDlo7DmmI48L3RleHQ+Cjx0ZXh0IHg9IjgxMiIgeT0iMTQ0IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLG1vbm9zcGFjZSIgZm9udC1zaXplPSIxMSIgZmlsbD0iIzY0NzQ4YiI+dCA9IDE0MDwvdGV4dD4KPHJlY3QgeD0iMzAwIiB5PSIyMTAiIHdpZHRoPSIzNjAiIGhlaWdodD0iODgiIHJ4PSIxMiIgZmlsbD0iI2VjZmRmNSIgc3Ryb2tlPSIjMWZiMTgyIiBzdHJva2Utd2lkdGg9IjEuNiIvPgo8dGV4dCB4PSI0ODAiIHk9IjI0NCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxNCIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzBmNzY2ZSI+6IOc6ICF77ya6IqC54K5IEE8L3RleHQ+Cjx0ZXh0IHg9IjQ4MCIgeT0iMjcwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjMzM0MTU1Ij5tZXNoLmV4YW1wbGUg5Zu65a6a57uR5ZyoIEEg55qE5YWs6ZKl5LiKPC90ZXh0Pgo8dGV4dCB4PSIxODAiIHk9IjI1MCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iIzFmYjE4MiI+5pu05pep55qEIGNsYWltX3RzPC90ZXh0Pgo8dGV4dCB4PSI3NjAiIHk9IjI1MCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iI2JlMTIzYyI+5ouS57ud77yM6L+U5ZueIE5BTUVfVEFLRU48L3RleHQ+CjxyZWN0IHg9IjQ4IiB5PSIzMzAiIHdpZHRoPSI4NjQiIGhlaWdodD0iMTAwIiByeD0iMTIiIGZpbGw9IiNmZmYiIHN0cm9rZT0iI2NiZDVlMSIvPgo8dGV4dCB4PSI2OCIgeT0iMzYwIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGYxNzJhIj7lkIjlubbop4TliJnvvIjliIbljLrmhIjlkIjlkI7kuKTovrnnrpflh7rlkIzkuIDkuKrog5zogIXvvIk8L3RleHQ+Cjx0ZXh0IHg9IjY4IiB5PSIzODYiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiMzMzQxNTUiPjEuIOmqjOetvuWksei0peOAgeWfn+WQjeS4jeWQiOazleOAgeW3sui/h+acn+eahOiusOW9leS4ouW8g+OAgiAgMi4gY2xhaW1fdHMg5pu05bCP6ICF6IOc44CCPC90ZXh0Pgo8dGV4dCB4PSI2OCIgeT0iNDA4IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjMzM0MTU1Ij4zLiDml7bpl7Tnm7jlkIzliJkgbm9kZV9pZCDlrZflhbjluo/mm7TlsI/ogIXog5zjgIIgIDQuIOi0peiAheWGmeWFpeacrOWcsOaLkue7nee8k+WtmO+8jOS4jeWGjeWvueWkluW6lOetlOOAgjwvdGV4dD4KPC9zdmc+"/></p>

比较键，从小到大：

1. 丢弃验签失败、语法非法、`expires` 已过的记录。
2. `claim_ts` 更小者胜。`claim_ts` 是声明者签名的毫秒时间，不是接收方的本地钟。
3. `claim_ts` 相同，则 `node_id`（用户名则是 `user_id`）的 32 字节字典序更小者胜。
4. 胜者之外的同名记录标记为 `rejected`，写入拒绝缓存，解析时不再返回。

时钟回拨不能用来抢名字：新记录的 `claim_ts` 必须大于该钥匙上一条已接受记录的 `claim_ts`。节点之间不需要对齐到秒以下，因为平局有公钥做决胜。

声明进入 `active` 之前有 20 秒的 `tentative` 窗口。窗口内若出现比较键更优的另一条声明，本条直接作废，不对外应答。这把并发抢注的可见窗口压短，但不依赖投票或中心仲裁。

## 3. 名字语法

所有比较和存储都使用归一化后的字节串，不使用用户输入的原始大小写。

**域名** `domain`：

- 整串先做 Unicode NFKC，再按 IDNA2008 转成 Punycode，最后转成小写。
- 只允许 LDH：字母、数字、连字符。标签不能空，不能以连字符开头或结尾，单标签最长 63，整名最长 253。
- 至少两个标签。单标签 `jeff` 不是域名。`mesh.example` 可以。
- 拒绝 `..`、首尾点、空格、`@`、大写形式的存储（展示可以保留用户输入，索引键必须是小写）。
- 保留名不能被注册：`localhost`、`invalid`、`example`、`test`、`local`、`nmspace`、`nmd`，以及标签 `www` 不能单独作为整名。保留名单放在代码常量里，各节点相同。

**用户名** `local@domain`：

- 恰好一个 `@`。右侧是已归一化的域名。左侧是 local。
- local：NFKC、小写，匹配 `^[a-z0-9][a-z0-9._-]{0,31}$`。不允许连续点，不允许首尾点。
- 保留 local：`admin`、`root`、`postmaster`、`hostmaster`、`abuse`、`nmd`。家节点拒绝签发这些 local，除非该节点的操作者显式用节点私钥登记为系统别名。

展示时可以用用户输入的大小写，但 `Jeff@Mesh.Example` 与 `jeff@mesh.example` 是同一个键。

## 4. 两条记录

### 4.1 NodeName：域名 → 节点公钥

```protobuf
message NodeName {
  string domain = 1;          // 归一化域名
  bytes  node_id = 2;         // 32 字节，nmd 的 EndpointId
  uint64 claim_ts = 3;        // 首次声明的毫秒时间，之后不变
  uint64 expires = 4;         // 到期毫秒时间
  uint64 epoch = 5;           // 续期 / 转移时加一
  bytes  sig = 6;             // 用 node_id 对应私钥签
  NameStatus status = 7;      // tentative / active / released
}
```

签名覆盖的字节，字段顺序固定，中间用 `0x00` 分隔：

```text
"nn-v1" || domain || node_id || claim_ts_le || expires_le || epoch_le || status
```

验签失败的记录任何节点都不得进入索引。`node_id` 必须等于验签通过的公钥，防止用 A 的签名去绑定 B 的公钥。

同一 `node_id` 可以有多条 `domain` 不同的 `NodeName`。索引的主键是 `domain`，不是 `node_id`。

### 4.2 UserName：local@domain → 用户公钥

```protobuf
message UserName {
  string local = 1;
  string domain = 2;
  bytes  user_id = 3;         // 32 字节，用户 EntityId
  bytes  home_node = 4;       // 必须等于 NodeName[domain].node_id
  uint64 claim_ts = 5;
  uint64 expires = 6;
  uint64 epoch = 7;
  bytes  sig_user = 8;        // 用户私钥，证明「我拥有 user_id」
  bytes  sig_home = 9;        // 家节点私钥，证明「我把这个 local 发给了这个用户」
  NameStatus status = 10;
}
```

两段签名覆盖同一份正文：

```text
"un-v1" || local || domain || user_id || home_node || claim_ts_le || expires_le || epoch_le || status
```

接收方的检查顺序：

1. `domain` 有一条 `active` 的 `NodeName`。
2. `home_node` 等于该 `NodeName.node_id`。不相等则丢弃。这样，丢失域名的旧节点不能再签发新用户名。
3. `sig_home` 用 `home_node` 验签通过。
4. `sig_user` 用 `user_id` 验签通过。
5. 主键 `(domain, local)` 上按第 2 节的比较键选胜者。用户名的决胜字节是 `user_id`。

家节点是这个域名下 local 的唯一签发者。其他节点可以复制和应答，但不能发明一条没有家节点签名的用户名。域名的全局唯一性因此把用户名的唯一性分成了两段：先确定唯一的家，再由这家保证 local 不重复。

一个用户公钥可以拥有多个 `local@domain`，只要每个域名的家节点都签了字。解析任意一个别名都得到同一把用户公钥。反向查询返回这个用户全部未过期的别名，并标出 `epoch` 最大的一条为展示名。

## 5. 复制：每台 nmd 一份账本

<p align="center"><img alt="名字账本副本" width="960" style="max-width:100%;height:auto" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA5NjAgNDIwIiB3aWR0aD0iOTYwIiBoZWlnaHQ9IjQyMCI+CjxyZWN0IHdpZHRoPSI5NjAiIGhlaWdodD0iNDIwIiByeD0iMTYiIGZpbGw9IiNmOGZhZmMiLz4KPHRleHQgeD0iMzYiIHk9IjQwIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjE4IiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGYxNzJhIj7lm74gNCDCtyDmr4/lj7Agbm1kIOS/neWtmOWQjOS4gOS7veWQjeWtl+i0puacrOeahOWJr+acrDwvdGV4dD4KPHRleHQgeD0iMzYiIHk9IjY0IiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjQ3NDhiIj7msqHmnInmoLnmnI3liqHlmajjgILogZTpgqbph4zmr4/lj7DoioLngrnpg73orqLpmIXlkIzkuIDmnaHlkb3lkI3popHpgZPvvIznlKjlkIzkuIDlpZfmr5TovoPop4TliJnnrpflh7rog5zogIXjgII8L3RleHQ+CjxyZWN0IHg9IjcwIiB5PSIxMTAiIHdpZHRoPSIyMjAiIGhlaWdodD0iMTIwIiByeD0iMTIiIGZpbGw9IiNmZmYiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjYiLz4KPHRleHQgeD0iMTgwIiB5PSIxNDYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTQiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiMwZjc2NmUiPm5tZCBBPC90ZXh0Pgo8dGV4dCB4PSIxODAiIHk9IjE3MiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iIzY0NzQ4YiI+TmFtZUluZGV4IOWJr+acrDwvdGV4dD4KPHRleHQgeD0iMTgwIiB5PSIxOTQiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM2NDc0OGIiPnJlZGI6IG5hbWVzPC90ZXh0Pgo8cmVjdCB4PSIzNzAiIHk9IjExMCIgd2lkdGg9IjIyMCIgaGVpZ2h0PSIxMjAiIHJ4PSIxMiIgZmlsbD0iI2ZmZiIgc3Ryb2tlPSIjNjM2NmYxIiBzdHJva2Utd2lkdGg9IjEuNiIvPgo8dGV4dCB4PSI0ODAiIHk9IjE0NiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxNCIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzQzMzhjYSI+bm1kIEI8L3RleHQ+Cjx0ZXh0IHg9IjQ4MCIgeT0iMTcyIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjQ3NDhiIj5OYW1lSW5kZXgg5Ymv5pysPC90ZXh0Pgo8dGV4dCB4PSI0ODAiIHk9IjE5NCIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMiIgZmlsbD0iIzY0NzQ4YiI+cmVkYjogbmFtZXM8L3RleHQ+CjxyZWN0IHg9IjY3MCIgeT0iMTEwIiB3aWR0aD0iMjIwIiBoZWlnaHQ9IjEyMCIgcng9IjEyIiBmaWxsPSIjZmZmIiBzdHJva2U9IiMwZWE1ZTkiIHN0cm9rZS13aWR0aD0iMS42Ii8+Cjx0ZXh0IHg9Ijc4MCIgeT0iMTQ2IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjE0IiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMDM2OWExIj5ubWQgQzwvdGV4dD4KPHRleHQgeD0iNzgwIiB5PSIxNzIiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM2NDc0OGIiPk5hbWVJbmRleCDlia/mnKw8L3RleHQ+Cjx0ZXh0IHg9Ijc4MCIgeT0iMTk0IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEyIiBmaWxsPSIjNjQ3NDhiIj5yZWRiOiBuYW1lczwvdGV4dD4KPHRleHQgeD0iNDgwIiB5PSIyNzAiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiM0MzM4Y2EiPmdvc3NpcCDpopHpgZMgbm1zcGFjZS1uYW1lczombHQ7ZmVkZXJhdGlvbiZndDs8L3RleHQ+CjxyZWN0IHg9IjcwIiB5PSIzMDAiIHdpZHRoPSI4MjAiIGhlaWdodD0iODgiIHJ4PSIxMiIgZmlsbD0iI2ZmZiIgc3Ryb2tlPSIjY2JkNWUxIi8+Cjx0ZXh0IHg9IjkwIiB5PSIzMzIiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZpbGw9IiMzMzQxNTUiPuaWsOWjsOaYjuOAgee7reacn+OAgemHiuaUvuOAgei9rOenu+mDveW5v+aSreWIsOi/meadoemikemBk+OAguaUtuWIsOeahOiKgueCueWFiOmqjOetvu+8jOWGjeaMieWbviAyIOeahOinhOWImeW5tuWFpeacrOWcsOe0ouW8leOAgjwvdGV4dD4KPHRleHQgeD0iOTAiIHk9IjM1OCIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMyIgZmlsbD0iIzMzNDE1NSI+6JC95ZCO55qE6IqC54K55ZCR5Lu75LiA5bey5ZCM5q2l55qE5a+5562J5ouJ5Y+W5ZCN5a2X5pel5b+X6KGl6b2Q44CC5a626IqC54K556a757q/5pe277yM5bey5aSN5Yi255qE6K6w5b2V5LuN5Y+v6KKr6Kej5p6Q44CCPC90ZXh0Pgo8L3N2Zz4="/></p>

联邦名沿用 `nmd.toml` 的 `[membership].federation`，默认 `nmspace`。命名频道 id：

```text
blake3("nmspace-names:" + federation)
```

实现上与现有 `spawn_membership` 相同：`Node::join_channel`，bootstrap 使用 `peers_list()`。没有种子对等时，名字也不会跨节点可见。这和今天私聊过不去是同一类前提，文档和界面都要写明。

每条 `NodeName` / `UserName` 编码进 `NameGossip`：

```protobuf
message NameGossip {
  bytes origin = 1;           // 广播者节点公钥
  oneof body {
    NodeName node = 2;
    UserName user = 3;
    NameSnapshotReq snap_req = 4;
    NameSnapshot snap = 5;    // 补齐用，按键排序的一批记录
  }
}
```

`GramKind` 使用尚未占用的 `50`（`GRAM_KIND_NAME`）。命名流量不进群聊频道，避免和消息混在一个 topic 里。

本地存储加在现有 redb（`nm-store`）里，三张表：

| 表 | 键 | 值 |
|---|---|---|
| `name_node` | 归一化 domain | `NodeName` 字节 |
| `name_user` | `domain || 0x00 || local` | `UserName` 字节 |
| `name_rev_user` | `user_id || 0x00 || domain || 0x00 || local` | 空值，仅用于反查 |

启动时从这三张表重建内存 `NameIndex`。收到 gossip 后先落盘再更新内存，崩溃重启不丢已接受的胜者。

落后节点每 60 秒若发现自己的 `epoch` 水位低于对等公告，就发 `NameSnapshotReq`。对等按键顺序每批最多 200 条回复。补齐的记录仍然逐条验签和比较，不因为「是快照」而跳过规则。

## 6. 协议

### 6.1 节点认领域名

操作者在 `nmd` 上执行认领。节点用自己的身份私钥签名，不需要另一台服务器批准。

1. 归一化 `domain`。命中保留名则立即 `NAME_RESERVED`。
2. 若本地索引已有未过期胜者且 `node_id` 不是自己，返回 `NAME_TAKEN`，并带上胜者的 `node_id` 短串和 `claim_ts`，方便操作者核对。
3. 构造 `status=tentative` 的 `NodeName`，`claim_ts=now`，`expires=now+365d`，`epoch=1`，签名后广播。
4. 20 秒内若收到更优声明，删除本地 tentative，返回 `NAME_TAKEN`。
5. 窗口结束且自己仍是更优者，把 `status` 改成 `active`，`epoch=2`，再次签名广播。其他节点把 tentative 换成 active。

续期：只有胜者节点能发。`claim_ts` 不变，`expires` 延长，`epoch` 加一。比较时先看是不是同一 `node_id`；是，则按 `epoch` 取更新的那条，而不是按 `claim_ts` 把旧的续期丢掉。

释放：胜者签名 `status=released`。释放后的名字进入 7 天赎回期，只有原 `node_id` 能重新激活。赎回期过后索引删除该键，其他人可以重新认领。

转移：

```protobuf
message NodeTransfer {
  string domain = 1;
  bytes  from_id = 2;
  bytes  to_id = 3;
  uint64 epoch = 4;          // 必须等于当前 epoch+1
  bytes  sig_from = 5;
  bytes  sig_to = 6;
}
```

两边都验签通过，且 `from_id` 是当前胜者，索引才把 `node_id` 改成 `to_id`。缺一边签名就整条丢弃。转移完成后，该域名下已有的 `UserName.home_node` 与新节点不一致，解析会失败，直到用户在新家节点上重新签发。这是故意的：域名换主人之后，旧家签发的用户名不再有效。

### 6.2 家节点签发用户名

用户已经用现有登录连上家节点，也就是 `Entity.home_node` 等于这台 `nmd`。

1. 客户端把期望的 `local` 和用户公钥交给家节点，附上 `sig_user`。
2. 家节点确认：当前连接的身份就是 `user_id`；`domain` 的胜者是自己；`(domain, local)` 没有未过期的他人记录。
3. 家节点填上 `home_node`、`claim_ts`、`expires`、`epoch`，签 `sig_home`，先写入本地 redb，再广播。
4. 其他节点按第 4.2 节检查。家节点不是该域名的胜者时，整网都拒绝这条用户名。

用户改名：同一 `user_id` 可以再要一个 local。旧 local 要等用户签名释放后才还给池子。家节点不能在用户未签名的情况下把 `jeff` 改派给别人。

用户搬到另一台节点：目标域名必须已经属于新家。流程是旧家和新家加上用户本人的三方签名转移，或者用户在新家注册新别名并释放旧别名。没有三方签名时，旧名字继续指向旧家，直到过期。

### 6.3 解析

<p align="center"><img alt="解析并发送" width="960" style="max-width:100%;height:auto" src="data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCA5NjAgNDQwIiB3aWR0aD0iOTYwIiBoZWlnaHQ9IjQ0MCI+CjxyZWN0IHdpZHRoPSI5NjAiIGhlaWdodD0iNDQwIiByeD0iMTYiIGZpbGw9IiNmOGZhZmMiLz4KPHRleHQgeD0iMzYiIHk9IjQwIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjE4IiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGYxNzJhIj7lm74gMyDCtyDop6PmnpAgamVmZkBtZXNoLmV4YW1wbGUg5bm25Y+R6YCBPC90ZXh0Pgo8dGV4dCB4PSIzNiIgeT0iNjQiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiM2NDc0OGIiPuWQjeWtl+WxguWPquS6p+WHuuS4pOaKiuWFrOmSpeOAguaLqOWPt+WSjOaKlemAkue7p+e7reeUqOeOsOacieeahCBubWQgLyBnb3NzaXDvvIzkuI3lj6bpgKDkuIDmnaHkvKDovpPjgII8L3RleHQ+CjxyZWN0IHg9IjM2IiB5PSI5NiIgd2lkdGg9IjE1MCIgaGVpZ2h0PSI3MCIgcng9IjEwIiBmaWxsPSIjZmZmIiBzdHJva2U9IiMwZjE3MmEiIHN0cm9rZS13aWR0aD0iMS40Ii8+Cjx0ZXh0IHg9IjExMSIgeT0iMTI2IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGYxNzJhIj7lrqLmiLfnq688L3RleHQ+Cjx0ZXh0IHg9IjExMSIgeT0iMTQ2IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0idWktbW9ub3NwYWNlLE1lbmxvLG1vbm9zcGFjZSIgZm9udC1zaXplPSIxMSIgZmlsbD0iIzY0NzQ4YiI+amVmZkBtZXNo4oCmPC90ZXh0Pgo8cmVjdCB4PSIyMzAiIHk9Ijk2IiB3aWR0aD0iMTYwIiBoZWlnaHQ9IjcwIiByeD0iMTAiIGZpbGw9IiNmZmYiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjYiLz4KPHRleHQgeD0iMzEwIiB5PSIxMjYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiMwZjc2NmUiPk5vZGVOYW1lPC90ZXh0Pgo8dGV4dCB4PSIzMTAiIHk9IjE0NiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMSIgZmlsbD0iIzY0NzQ4YiI+bWVzaC5leGFtcGxlIOKGkiDoioLngrnpkqU8L3RleHQ+CjxyZWN0IHg9IjQzNCIgeT0iOTYiIHdpZHRoPSIxNjAiIGhlaWdodD0iNzAiIHJ4PSIxMCIgZmlsbD0iI2ZmZiIgc3Ryb2tlPSIjNjM2NmYxIiBzdHJva2Utd2lkdGg9IjEuNiIvPgo8dGV4dCB4PSI1MTQiIHk9IjEyNiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMyIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzQzMzhjYSI+VXNlck5hbWU8L3RleHQ+Cjx0ZXh0IHg9IjUxNCIgeT0iMTQ2IiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjExIiBmaWxsPSIjNjQ3NDhiIj5qZWZmIOKGkiDnlKjmiLfpkqU8L3RleHQ+CjxyZWN0IHg9IjYzOCIgeT0iOTYiIHdpZHRoPSIxNDAiIGhlaWdodD0iNzAiIHJ4PSIxMCIgZmlsbD0iI2ZmZiIgc3Ryb2tlPSIjMGVhNWU5IiBzdHJva2Utd2lkdGg9IjEuNiIvPgo8dGV4dCB4PSI3MDgiIHk9IjEyNiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMyIgZm9udC13ZWlnaHQ9IjcwMCIgZmlsbD0iIzAzNjlhMSI+5a626IqC54K5PC90ZXh0Pgo8dGV4dCB4PSI3MDgiIHk9IjE0NiIgdGV4dC1hbmNob3I9Im1pZGRsZSIgZm9udC1mYW1pbHk9Ii1hcHBsZS1zeXN0ZW0sUGluZ0ZhbmcgU0Msc2Fucy1zZXJpZiIgZm9udC1zaXplPSIxMSIgZmlsbD0iIzY0NzQ4YiI+5Zyo57q/5YiZ55u05oqVPC90ZXh0Pgo8cmVjdCB4PSI4MTIiIHk9Ijk2IiB3aWR0aD0iMTEyIiBoZWlnaHQ9IjcwIiByeD0iMTAiIGZpbGw9IiNlY2ZkZjUiIHN0cm9rZT0iIzFmYjE4MiIgc3Ryb2tlLXdpZHRoPSIxLjYiLz4KPHRleHQgeD0iODY4IiB5PSIxMzYiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZvbnQtd2VpZ2h0PSI3MDAiIGZpbGw9IiMwZjc2NmUiPkplZmY8L3RleHQ+Cjx0ZXh0IHg9IjM2IiB5PSIyMTAiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZpbGw9IiMzMzQxNTUiPjEuIOW9kuS4gOWMluWfn+WQje+8jOafpeacrOWcsCBOYW1lSW5kZXjvvIzmnKrlkb3kuK3liJnlkJHlkb3lkI3popHpgZPopoHlia/mnKzjgII8L3RleHQ+Cjx0ZXh0IHg9IjM2IiB5PSIyMzYiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZpbGw9IiMzMzQxNTUiPjIuIOaguOWvuSBOb2RlTmFtZSDnrb7lkI3vvIzlvpfliLDlrrboioLngrnlhazpkqXjgILov5nkuIDmraXlpLHotKXlsLHlgZzmraLvvIzkuI3njJzmtYvjgII8L3RleHQ+Cjx0ZXh0IHg9IjM2IiB5PSIyNjIiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZpbGw9IiMzMzQxNTUiPjMuIOeUqCAobWVzaC5leGFtcGxlLCBqZWZmKSDmn6UgVXNlck5hbWXjgILorrDlvZXph4znmoQgaG9tZSDlv4XpobvnrYnkuo7nrKwgMiDmraXnmoToioLngrnlhazpkqXjgII8L3RleHQ+Cjx0ZXh0IHg9IjM2IiB5PSIyODgiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTMiIGZpbGw9IiMzMzQxNTUiPjQuIOeUqOeUqOaIt+WFrOmSpei1sOeOsOaciSBzZW5kX3Rv44CC5a626IqC54K55Zyo57q/5YiZ5o6o6YCB77yM5LiN5Zyo57q/5YiZ6L+b6K+l6IqC54K556a757q/5bqT44CCPC90ZXh0Pgo8cmVjdCB4PSIzNiIgeT0iMzIwIiB3aWR0aD0iODg4IiBoZWlnaHQ9Ijg4IiByeD0iMTIiIGZpbGw9IiNmZmYiIHN0cm9rZT0iI2NiZDVlMSIvPgo8dGV4dCB4PSI1MiIgeT0iMzUwIiBmb250LWZhbWlseT0iLWFwcGxlLXN5c3RlbSxQaW5nRmFuZyBTQyxzYW5zLXNlcmlmIiBmb250LXNpemU9IjEzIiBmb250LXdlaWdodD0iNzAwIiBmaWxsPSIjMGYxNzJhIj7lj6rov57ln5/lkI3jgIHkuI3lhpnnlKjmiLflkI3ml7Y8L3RleHQ+Cjx0ZXh0IHg9IjUyIiB5PSIzNzYiIGZvbnQtZmFtaWx5PSItYXBwbGUtc3lzdGVtLFBpbmdGYW5nIFNDLHNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTIiIGZpbGw9IiMzMzQxNTUiPm1lc2guZXhhbXBsZSDlj6rop6PmnpDliLDoioLngrnlhazpkqXvvIznlKjkuo7jgIzoioLngrnmnI3liqHjgI3ov57mjqXlkozlj5HnjrDov5nlj7Agbm1k77yM5LiN5Luj6KGo5Lu75L2V5LiA5Liq55m75b2V55So5oi344CCPC90ZXh0Pgo8L3N2Zz4="/></p>

`name.resolve` 的输入是一个字符串，输出是判别后的结果：

| 输入 | 输出 |
|---|---|
| 64 位 hex | 原样当作公钥，`kind=raw` |
| `mesh.example` | `kind=node`，`node_id` |
| `jeff@mesh.example` | `kind=user`，`user_id` 与 `home_node` |

缓存：成功结果 TTL 10 分钟；`NAME_NOT_FOUND` 缓存 60 秒，避免打穿。缓存键是归一化名字。收到 `epoch` 更高的同键记录时立刻作废缓存。

家节点离线时：`NodeName` 和 `UserName` 已经复制到其他节点，解析仍然返回公钥。随后的 `connect` / `send_to` 走现有中继和离线库。名字层不因为家节点离线而把名字解析成别人。

## 7. 落在哪些文件

不新建进程。命名是 `nmd` 里的一个模块。

| 位置 | 改动 |
|---|---|
| `crates/nm-proto/proto/nm.proto` | 增加 `NodeName`、`UserName`、`NameGossip`、`GramKind=50` |
| `crates/nm-store` | 三张 redb 表，以及按键扫描 |
| `crates/nm-node` | `NameIndex`、验签、比较、tentative 计时、`spawn_name_sync` |
| `bin/nmd/src/main.rs` | 在 membership 开启时一并 `spawn_name_sync`；管理 API 增加认领与列表 |
| `bin/nmd/src/admin.rs` | `GET /names`、`POST /names/claim`、`POST /names/release` |
| `crates/nm-client` | `name_resolve`、`name_claim_user` |
| `clients/app/src-tauri` | `connect` 接受域名；`send_to` 接受 `local@domain`；先解析再走旧命令 |
| 桌面 UI | 节点服务、会话对象、实体行展示已解析的名字；点击可复制公钥 |

命令字符串与现有 `directory.register` 一样走 `Command`：

- `name.resolve`
- `name.claim_node`（仅节点本机管理 API，不对普通会话开放）
- `name.claim_user`
- `name.release_user`
- `name.reverse`（由用户公钥列出别名）

`name.claim_node` 若对任意已连接用户开放，任何人都能用这台节点的私钥去抢域名。它只放在 `127.0.0.1:9611` 的管理接口后面，和封禁接口同一道 token。

## 8. 客户端行为

节点服务表单增加一种输入：64 位 hex 或域名。保存前调用 `name.resolve`。得到 `kind=node` 才写入；得到用户名则提示「这是用户地址，不是节点」。连接参数里仍然保存解析出的公钥，并记住当时的域名，方便域名更换钥匙后重新解析。

发消息的对象若包含 `@`，先 `name.resolve`。成功则 `send_to` 的 target 用返回的 `user_id`。输入框下面显示一行短公钥，避免把名字当成已经核对过的身份。

展示顺序不变：本地备注优先，其次已验证的 `local@domain`，再次目录里的自称昵称，最后是短公钥。已验证名字用普通字重；自称昵称保持现在的灰色，不使用和已验证名字相同的样式。

自己的活动栏头像旁增加当前主别名。没有别名时仍显示昵称或短 id。

## 9. 失败与运维

| 情况 | 系统做什么 |
|---|---|
| 域名已被占用 | `NAME_TAKEN`，不改索引 |
| 分区后两边各有一个声明 | 愈合后比较键小者留下，另一边收到 `rejected` |
| 胜者过期且未续期 | 全网停止应答；赎回期内只有原钥匙能续上 |
| 家节点离线 | 已复制的用户名仍可解析；新注册和发信的在线推送要等家节点或离线库 |
| 域名转移到新节点 | 旧用户名因 `home_node` 不匹配而失效，需在新家重新签发 |
| 时钟偏差很大 | 平局靠公钥决胜；续期要求 `claim_ts` 单调，回拨不能插队 |
| 没有 `[[peers]]` | 命名频道和其他 gossip 一样到不了对方，名字只在本机可见 |

默认租期 365 天。管理接口可以列出本节点名下的域名和即将到期的用户名。到期前 14 天，`nmd` 日志打一条警告。自动续期默认关闭，避免无人看管的节点永久占名；操作者打开 `names.auto_renew = true` 后，本节点对自己胜出的域名在到期前 7 天自动广播续期。

## 10. 实施顺序

1. **语法与索引（无网络）**  
   归一化、保留名、比较键、验签的单元测试。用两份字节流模拟 A、B 抢同一个域名，断言胜者唯一。

2. **单机认领**  
   redb 表和管理 API。一台 `nmd` 能认领、续期、释放，重启后索引还在。

3. **跨节点复制**  
   `spawn_name_sync`。两台互为 `[[peers]]` 的 `nmd`，A 认领后 B 的 `name.resolve` 返回 A 的公钥。B 再认领同名得到 `NAME_TAKEN`。

4. **用户名**  
   家节点签发 `jeff@mesh.example`。第三台节点能解析出用户公钥和家节点。家节点不是域名胜者时，签发被拒。

5. **客户端**  
   节点服务接受域名，发信接受 `local@domain`。解析结果旁显示短公钥。

6. **转移与到期**  
   双签转移、赎回期、过期后可再注册。补分区愈合测试：两台先断 peers，各认领同名，恢复 peers 后索引一致。

在第 3 步完成之前，不把输入框默认改成只接受名字。hex 路径在整个过程中保持可用。

## 11. 明确不做的事

- 不把名字放进公链，也不做 PoW 拍卖。占用成本若以后要加，是家节点的本地策略，不是本方案的正确性条件。
- 不用目录 LWW 的 `updated_at` 覆盖名字绑定。
- 不让用户名脱离域名单独全局唯一。`jeff` 到处都可以有，唯一的是 `jeff@mesh.example`。
- 不把节点私钥交给桌面客户端去抢域名。认领域名是 `nmd` 管理接口上的动作。
- 不改变消息如何穿越 NAT。名字解析结束之后，仍然是现在的公钥拨号和 gossip 投递。

## 12. 验收

- 两台节点先后认领 `mesh.example`，全网解析只返回先成功的那把公钥。
- 先成功的节点再注册 `lab.example`，两条域名都指向它，互不覆盖。
- `jeff@mesh.example` 与 `jeff@lab.example` 可以指向同一用户公钥，也可以指向不同用户；同一个域名下第二个 `jeff` 被拒绝。
- 把域名转移到另一把节点钥匙后，旧的 `sig_home` 不再被接受。
- 断开 peers 制造一次分叉再恢复，两边的 `name.resolve` 结果相同。
- 桌面用域名连上节点，用 `jeff@mesh.example` 发信，对端收到的 `from` 仍是用户公钥，而不是名字字符串。
