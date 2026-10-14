// node-link 实例模式前端锚（gateway-node-p1 任务 7/9）：
// 1. instanceBaseUrl：node-link 走同源代理前缀（零改动成立的实证——非 direct
//    分支自动覆盖）；同源前缀下 wsEndpoint 升 ws + hub token，事件流 URL 恰为
//    /api/instances/{id}/p/ws/events（T9 在线信号通道的消费路径）
// 2. 离线复活守卫：node-link ctx.mode ≠ 'ssh-tunnel' → tunnelStart 自动复活
//    循环不会命中（任务 9 确认点；与 ssh-tunnel 守卫对照）

import { describe, expect, it } from 'vitest';
import { wsEndpoint } from './api';
import { instanceBaseUrl, ctxOf } from './instances';
import type { InstanceConfig } from './types';

function nodeLinkInst(): InstanceConfig {
  return {
    id: 'n-1',
    name: 'gateway-node',
    mode: 'node-link',
    url: null,
    token: null,
    ssh: null,
    remotePort: null,
    localPort: null,
    nodeMeta: { platform: 'macos-aarch64', drivers: ['claude', 'dsh'] },
  };
}

describe('node-link 实例模式（gateway-node-p1）', () => {
  it('instanceBaseUrl 走同源代理前缀（任务 7 锚）', () => {
    expect(instanceBaseUrl(nodeLinkInst())).toBe('/api/instances/n-1/p');
  });

  it('同源前缀 → wsEndpoint 升 ws + 事件流 URL 恰为 /p/ws/events（任务 9 消费路径）', () => {
    const baseUrl = instanceBaseUrl(nodeLinkInst()) ?? '';
    const { host, token } = wsEndpoint(baseUrl, 'n-1');
    expect(host.startsWith('ws://') || host.startsWith('wss://')).toBe(true);
    expect(host).toContain('/api/instances/n-1/p');
    const eventsUrl = `${host}/ws/events`;
    expect(eventsUrl).toBe(
      `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/api/instances/n-1/p/ws/events`,
    );
    expect(token === null || typeof token === 'string').toBe(true);
  });

  it('ctxOf node-link：mode 非 ssh-tunnel → 离线不触发 tunnelStart 复活（任务 9 守卫）', () => {
    const ctx = ctxOf(nodeLinkInst());
    expect(ctx.mode).toBe('node-link');
    expect(ctx.mode === 'ssh-tunnel').toBe(false);
    // 对照：ssh-tunnel ctx 守卫命中（复活循环只筛 ssh-tunnel）
    const sshCtx = ctxOf({
      id: 's-1',
      name: 'ssh',
      mode: 'ssh-tunnel',
      url: null,
      token: null,
      ssh: null,
      remotePort: 7800,
      localPort: null,
      nodeMeta: null,
    });
    expect(sshCtx.mode === 'ssh-tunnel').toBe(true);
  });
});
