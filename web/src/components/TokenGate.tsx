import { useEffect, useRef, useState, type ReactElement } from 'react';
import QRCode from 'qrcode';
import { captureUrlToken, storeToken } from '../api';
import './TokenGate.css';

/// allow_lan 模式遇 401 时的门禁：token 输入 / 二维码扫码。
/// localhost 默认模式不触发（无 401）。
export default function TokenGate({ onUnlocked }: { onUnlocked: () => void }): ReactElement {
  const [token, setToken] = useState(sessionStorage.getItem('hub_token') ?? '');
  const [err, setErr] = useState('');
  const [qrText, setQrText] = useState('');
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  // URL ?token= 直接解锁（手机扫码入口）
  useEffect(() => {
    if (captureUrlToken()) {
      onUnlocked();
    }
  }, [onUnlocked]);

  const showQr = async (): Promise<void> => {
    setErr('');
    try {
      const res = await fetch('/api/auth/token', {
        headers: token.trim() ? { authorization: `Bearer ${token.trim()}` } : {},
      });
      if (!res.ok) {
        setErr('获取 token 失败（局域网模式未开启或非本机访问）');
        return;
      }
      const { token: t } = (await res.json()) as { token: string };
      // 二维码内容 = 带 token 的本机访问地址（hostname 取当前访问地址）
      const url = `${location.protocol}//${location.host}/?token=${encodeURIComponent(t)}`;
      setQrText(url);
      if (canvasRef.current) {
        await QRCode.toCanvas(canvasRef.current, url, { width: 220 });
      }
    } catch (e) {
      setErr(String(e));
    }
  };

  const save = (): void => {
    const t = token.trim();
    if (!t) {
      setErr('请输入 token');
      return;
    }
    // 反馈轮 10：持久化到 localStorage（跨浏览器重启免输）；失效由 401 清除兜底
    storeToken(t);
    onUnlocked();
  };

  return (
    <div className="token-gate" role="dialog" aria-label="Token gate">
      <div className="token-card">
        <h2>需要访问令牌</h2>
        <p className="token-hint">
          已开启局域网访问。输入本机 <code>~/.claude-view/token</code> 中的 token，
          或在本机打开此页面生成二维码扫码。
        </p>
        <input
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder="access token"
          aria-label="access token"
          autoComplete="off"
        />
        {err ? <p className="dialog-error">{err}</p> : null}
        <div className="token-actions">
          <button type="button" onClick={save}>
            保存并进入
          </button>
          <button type="button" onClick={() => void showQr()}>
            生成二维码（本机）
          </button>
        </div>
        {qrText ? (
          <div className="token-qr">
            <canvas ref={canvasRef} />
            <p className="token-hint">{qrText}</p>
          </div>
        ) : null}
      </div>
    </div>
  );
}
