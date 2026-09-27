import { useCallback, useEffect, useMemo, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { api as localApi, chatConfig, type MessagePage, type Api } from '../api';
import type { ChatEvent, ChatMessage } from '../types';

const EMPTY_PAGE: MessagePage = { messages: [], firstLine: 0, hasMore: false };

/// WS host 解析：本机（baseAppUrl=''）→ location.host + 协议升 wss；远程 → baseUrl 协议升 ws。
function wsHostFor(apiLike: { baseAppUrl: string }): string {
  if (!apiLike.baseAppUrl) {
    return `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}`;
  }
  const proto = apiLike.baseAppUrl.startsWith('https:') ? 'wss:' : 'ws:';
  return apiLike.baseAppUrl.replace(/^https?:/, proto);
}

/// 向前翻页状态（B13）：pages 首元素最旧；cursor = 已拉到的最老游标（淘汰清页后仍有效，
/// 防游标回退重复拉取）；hasMore 独立记录——翻过页后即使 pages 被淘汰语义不变
interface OlderState {
  pages: MessagePage[];
  cursor: number | null;
  hasMore: boolean;
}
const OLDER_INIT: OlderState = { pages: [], cursor: null, hasMore: false };

/// 会话数据（P6 B12/B13 分页）：历史走分页端点——末页 react-query（key 按 agentId+subagentId），
/// 向前翻页的更早页存在本地页数组（older.pages，首元素最旧）；WS /ws/chat/{sessionId}
/// 实时增量直接写 query cache 末页（review-r1 合流模式，追加尾部不冲突）。
/// 历史 query 关闭自动 refetch（staleTime Infinity）：末页窗口覆盖会丢已翻页/已追加历史，
/// 失败重试走手动 refetch（仅空态可见）。
/// source='acp'（批2 任务8）：历史走 /api/acp/sessions/{id}/messages（服务端内存回放，
/// 信封同构），无翻页；WS 房间复用同一 /ws/chat/{sessionId}。
export function useSession(
  agentId: string | null,
  sessionId: string | null,
  subagentId: string | null = null,
  source: 'claude' | 'acp' = 'claude',
  instanceId: string | null = null,
  api: Api | null = null,
): {
  messages: ChatMessage[];
  isLoading: boolean;
  error: Error | null;
  refetch: () => void;
  hasMore: boolean;
  noMore: boolean;
  loadingOlder: boolean;
  loadOlder: () => Promise<boolean>;
} {
  const qc = useQueryClient();
  // 惰性取本机 api：测试环境的 api mock 常无 makeApi，未经注入时用模块级 api
  const instApi: Api = api ?? localApi;
  // ocr-review 高：source 纳入 queryKey——同 agentId 在驱动切换时复用缓存会
  // ACP/Claude 消息互相污染；读写（setQueryData / older 重置 / loadOlder / WS）同 key
  // instanceId 纳入 queryKey：同 agentId/driver 跨实例隔离（漏一处串一处）
  const queryKey = useMemo(
    () => ['messages', instanceId, agentId, subagentId, source] as const,
    [instanceId, agentId, subagentId, source],
  );
  const { data, isLoading, error, refetch } = useQuery<MessagePage>({
    queryKey,
    queryFn: () => {
      if (source === 'acp') return instApi.acpMessages(agentId as string);
      return subagentId
        ? instApi.subagentMessagesPage(agentId as string, subagentId)
        : instApi.messagesPage(agentId as string);
    },
    enabled: agentId != null,
    staleTime: Infinity,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });

  const [older, setOlder] = useState<OlderState>(OLDER_INIT);
  const [loadingOlder, setLoadingOlder] = useState(false);
  useEffect(() => {
    setOlder(OLDER_INIT);
    setLoadingOlder(false);
  }, [agentId, subagentId, source]);

  const page = data ?? EMPTY_PAGE;
  const messages = useMemo(
    () => [...older.pages.flatMap((p) => p.messages), ...page.messages],
    [older.pages, page.messages],
  );

  // 缓冲上限（B12）：config 启动取一次（服务端默认 20/100，重启生效）；
  // 用 state 而非 ref——config 晚到也要触发淘汰重算
  const [bufferMax, setBufferMax] = useState(100);
  useEffect(() => {
    let alive = true;
    void chatConfig().then((c) => {
      if (alive) setBufferMax(c.bufferMax);
    });
    return () => {
      alive = false;
    };
  }, []);

  // 缓冲淘汰（B12）：总量超上限 → 从最旧整页淘汰至 ≤ 上限（只清 older.pages，
  // cursor/hasMore 保留——翻页游标不回退不重复）
  useEffect(() => {
    setOlder((o) => {
      let total = o.pages.reduce((s, p) => s + p.messages.length, 0) + page.messages.length;
      // pages 为空时必须返回原引用：否则 setState 新对象 → effect 重跑 → 死循环白屏（r1 B-1）
      if (total <= bufferMax || o.pages.length === 0) return o;
      const pages = [...o.pages];
      while (pages.length > 0 && total > bufferMax) {
        total -= pages[0].messages.length;
        pages.shift();
      }
      return { ...o, pages };
    });
  }, [older, page.messages, bufferMax]);

  const hasMore = older.cursor != null ? older.hasMore : page.hasMore;
  const noMore = older.cursor != null && !older.hasMore;

  // 向前翻页（B13）：游标 = 已拉到的最老行号（未翻页时为末页游标）；返回是否取到消息
  const loadOlder = useCallback(async (): Promise<boolean> => {
    // ocr-review 高：ACP 无 claude 分页端点（内存回放模型），短路防 404
    if (source === 'acp') return false;
    if (loadingOlder || agentId == null) return false;
    const cursor = older.cursor ?? page.firstLine;
    if (cursor <= 0) return false;
    setLoadingOlder(true);
    try {
      const pg = await (subagentId
        ? instApi.subagentMessagesPage(agentId, subagentId, cursor)
        : instApi.messagesPage(agentId, cursor));
      setOlder((o) => ({ pages: [{ ...pg }, ...o.pages], cursor: pg.firstLine, hasMore: pg.hasMore }));
      return pg.messages.length > 0;
    } finally {
      setLoadingOlder(false);
    }
  }, [source, loadingOlder, agentId, subagentId, older.cursor, page.firstLine, instApi]);

  useEffect(() => {
    if (sessionId == null || agentId == null) return;
    let ws: WebSocket | null = null;
    let closed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const connect = (): void => {
      if (closed) return;
      const wsBase = wsHostFor(instApi);
      const t = instanceId == null ? sessionStorage.getItem('hub_token') : sessionStorage.getItem(`hub_token_${instanceId}`);
      const q = t ? `?token=${encodeURIComponent(t)}` : '';
      const channel = subagentId ? `${sessionId}/subagents/${subagentId}` : sessionId;
      const url = `${wsBase}/ws/chat/${channel}${q}`;
      ws = new WebSocket(url);
      ws.onmessage = (m): void => {
        try {
          const ev = JSON.parse(m.data as string) as ChatEvent;
          qc.setQueryData<MessagePage>(
            queryKey,
            (prev = EMPTY_PAGE) => ({ ...prev, messages: [...prev.messages, ev.message] }),
          );
        } catch {
          // 坏帧忽略
        }
      };
      ws.onclose = (): void => {
        if (!closed) timer = setTimeout(connect, 2_000);
      };
    };
    connect();

    return () => {
      closed = true;
      if (timer != null) clearTimeout(timer);
      ws?.close();
    };
  }, [sessionId, agentId, subagentId, source, queryKey, qc, instApi, instanceId]);

  // isLoading：react-query 语义——缓存命中秒开即为 false（防闪烁），
  // 仅真实等待（无缓存且在途）为 true；error/refetch 供消息区失败重试
  return {
    messages,
    isLoading: isLoading && agentId != null,
    error: error ?? null,
    refetch: () => {
      void refetch();
    },
    hasMore,
    noMore,
    loadingOlder,
    loadOlder,
  };
}
