import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import App from './App';
import { initI18n } from './i18n';
import './index.css';
import './bui/bui.css';
import './bui/bui-motion.css';

initI18n(); // 早于首屏渲染同步 <html lang>，防闪烁（agent-hub-settings C1）

const client = new QueryClient();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
