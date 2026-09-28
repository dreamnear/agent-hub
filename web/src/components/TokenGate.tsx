import { useEffect, useRef, useState, type ReactElement } from 'react';
import QRCode from 'qrcode';
import { captureUrlToken, storeToken } from '../api';
import { useI18n } from '../i18n';
import './TokenGate.css';

/// allow_lan 模式遇 401 时的门禁：token 输入 / 二维码扫码。
/// localhost 默认模式不触发（无 401）。
export default function TokenGate({ onUnlocked }: { onUnlocked: () => void }): ReactElement {
  const t = useI18n();
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
        setErr(t('gate.qrFail'));
        return;
      }
      const { token: qrToken } = (await res.json()) as { token: string };
      // 二维码内容 = 带 token 的本机访问地址（hostname 取当前访问地址）
      const url = `${location.protocol}//${location.host}/?token=${encodeURIComponent(qrToken)}`;
      setQrText(url);
      if (canvasRef.current) {
        await QRCode.toCanvas(canvasRef.current, url, { width: 220 });
      }
    } catch (e) {
      setErr(String(e));
    }
  };

  const save = (): void => {
    const tok = token.trim();
    if (!tok) {
      setErr(t('gate.enterToken'));
      return;
    }
    // 反馈轮 10：持久化到 localStorage（跨浏览器重启免输）；失效由 401 清除兜底
    storeToken(tok);
    onUnlocked();
  };

  return (
    <div className="token-gate" role="dialog" aria-label="Token gate">
      <div className="token-card">
        <h2>{t('gate.title')}</h2>
        <p className="token-hint">
          {t('gate.hint1')} <code>~/.claude-view/token</code>
          {t('gate.hint2')}
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
            {t('gate.save')}
          </button>
          <button type="button" onClick={() => void showQr()}>
            {t('gate.qr')}
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
