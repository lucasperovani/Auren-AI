- Tasks precisa ser transformada em um kanban (Analysis, To Do, In Progress, Done) ✅ (coluna `stage` independente do `status`, drag & drop + setas do teclado, auto-avanço no run)
- Projetos: tasks isoladas por projeto, criadas pela pessoa ou pela IA ✅ (`projects` + `project_id`, board por projeto, `project_create`/`project_list` na IA)
- Referências entre cards ✅ (`task_links` com `blocks`/`relates`, atribuição recusa task bloqueada)
- IA pode criar/mover/notar/ligar cards e se auto-atribuir ✅ (`task_move`, `task_assign`, `task_note`, `task_link`, `task_unlink`)
- Ver o que a IA executou numa task ✅ (painel de detalhe com abas Overview / What the agent did / Runs, timeline com tool calls)

- Listar/Puxar skills/tools/mcp de um outro github ✅ (scan + install via `POST /skills/import/discover|install`, UI em Skills)
- Ver se o instagram está integrado corretamente e funcionando e valide para ver se há limitação de uso da API (200 mensagens por hora, 50 a 100 posts por dia. Se estourar o limite, colocar nas notificações e tentar assim que o limite passar. Se possível, não faça pooling, se tiver que fazer, use backoff exponencial e não deixe a IA ficar fazendo o polling), marcar is_ai_generated ✅ (publish corrigido para `/media_publish`, container status, `is_ai_generated`, rate limits com deferral + backoff, sem polling da IA)
- Precisa haver um esquema de notificação que a pessoa pode ver, ler, deletar notificações. A IA ou o sistema pode mandar essas notificações. Dependendo, pode passar para a IA agir sobre a notificação. A notificação precisa ter título, descrição e pode ter botões de ação ✅ (título, corpo, severidade, ações, badge de não lidas, ações despacham para o agente)

- As tasks não podem mostrar o formato do tempo como no cron, precisa ser algo bonitinho
- Corrigir/Remover testes flakiness
- Melhorar e muito a interface