- Tasks precisa ser transformada em um kanban (Analysis, To Do, In Progress, Done)
- Listar/Puxar skills/tools/mcp de um outro github
- Ver se o instagram está integrado corretamente e funcionando e valide para ver se há limitação de uso da API (200 mensagens por hora, 50 a 100 posts por dia. Se estourar o limite, colocar nas notificações e tentar assim que o limite passar. Se possível, não faça pooling, se tiver que fazer, use backoff exponencial e não deixe a IA ficar fazendo o polling), marcar is_ai_generated
- Precisa haver um esquema de notificação que a pessoa pode ver, ler, deletar notificações. A IA ou o sistema pode mandar essas notificações. Dependendo, pode passar para a IA agir sobre a notificação. A notificação precisa ter título, descricação e pode ter botões de ação

- As tasks não podem mostrar o formato do tempo como no cron, precisa ser algo bonitinho
- Corrigir/Remover testes flakiness
- Melhorar e muito a interface