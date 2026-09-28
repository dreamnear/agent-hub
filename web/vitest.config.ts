import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'happy-dom',
    setupFiles: ['./src/test-setup.ts'],
    css: { include: /(TaskListBar|ChatTab|SubagentBar|ProjectSidebar|GitPanel|DocViewer)\.css$/ },
  },
});
