import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

// 设置页「关于」分区的构建期注入（零服务端改动）：版本取 web/package.json，
// 提交取 git 短 sha；无 git 环境（如源码分发）回落 unknown。
const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as {
  version: string;
};
const gitSha = ((): string => {
  try {
    return execSync('git rev-parse --short HEAD', { encoding: 'utf8' }).trim();
  } catch {
    return 'unknown';
  }
})();

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: {
    __HUB_VERSION__: JSON.stringify(pkg.version),
    __HUB_GIT_SHA__: JSON.stringify(gitSha),
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:7800',
      '/ws': {
        target: 'ws://127.0.0.1:7800',
        ws: true,
      },
    },
  },
});
