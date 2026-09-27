// 多实例前端数据层（agent-hub-multi-instance 批1 任务4）：实例清单加载、per-instance
// api 工厂解析。原版同时做实例清单 + apiForCtx；为避免与 useAgents 循环依赖，只保留
// 纯数据/工厂函数（不 import api 的具体 Api 类型，用本地最小形状）。

import { makeApi } from './api';
import type { Api } from './api';
import type { InstanceConfig } from './types';

/// 本机实例 id 约定：null（token 走全局 hub_token，相对路径）。
export const LOCAL_INSTANCE_ID: string | null = null;

/// 本机 hub api（实例管理端点也走相对路径 + hub token 认证）。
/// ponytail: 惰性构造——模块加载期不触碰 api mock（测试环境 ./api mock 常无 makeApi），
/// 首次 fetchInstances 才创建（单例缓存）。
let localHubApiCache: Api | null = null;
function localHubApi(): Api {
  localHubApiCache ??= makeApi('', LOCAL_INSTANCE_ID);
  return localHubApiCache;
}

/// 遍历用的实例上下文：本机 + 各远程配置。
export interface InstanceCtx {
  /** 实例 id；null = 本机 */
  id: string | null;
  name: string;
  mode: InstanceConfig['mode'];
  /** API/WS base url：本机=''（相对路径），ssh-tunnel=http://127.0.0.1:{localPort} */
  baseUrl: string;
  /** 原始配置（远程）；本机为 null */
  config: InstanceConfig | null;
}

const instancesPromiseCache = new Map<string, Promise<InstanceConfig[]>>();

/** 拉取实例清单（自本机 hub 实例管理器）。同一批调用共享同一 Promise。 */
export function fetchInstances(): Promise<InstanceConfig[]> {
  const hit = instancesPromiseCache.get('instances');
  if (hit) return hit;
  const p = localHubApi()
    .listInstances()
    .finally(() => instancesPromiseCache.delete('instances'));
  instancesPromiseCache.set('instances', p);
  return p;
}

/** 解析远程实例 base url：direct → url;ssh-tunnel → http://127.0.0.1:{localPort}（未就绪 null） */
export function instanceBaseUrl(inst: InstanceConfig): string | null {
  if (inst.mode === 'direct') return inst.url;
  return inst.localPort != null ? `http://127.0.0.1:${inst.localPort}` : null;
}

/** 由远程配置构造实例上下文。ssh-tunnel 未分配 localPort → baseUrl=''（调用方跳过/示离线） */
export function ctxOf(inst: InstanceConfig): InstanceCtx {
  return {
    id: inst.id,
    name: inst.name,
    mode: inst.mode,
    baseUrl: instanceBaseUrl(inst) ?? '',
    config: inst,
  };
}

/** 本机实例上下文（baseUrl='' 相对路径）。 */
export function localCtx(): InstanceCtx {
  return { id: LOCAL_INSTANCE_ID, name: '本机', mode: 'direct', baseUrl: '', config: null };
}

/** 每实例 api（本机 = 空 base 相对路径）。 */
export function apiForCtx(ctx: InstanceCtx): Api {
  return makeApi(ctx.baseUrl, ctx.id);
}

/** 便捷：按实例清单 id 取上下文（未知 → 本机）。 */
export function ctxById(id: string | null, instances: InstanceCtx[]): InstanceCtx {
  return instances.find((c) => c.id === id) ?? localCtx();
}