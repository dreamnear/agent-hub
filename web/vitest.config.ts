import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'happy-dom',
    css: { include: /(TaskListBar|ChatTab|SubagentBar|ProjectSidebar|GitPanel|DocViewer)\.css$/ },
  },
});
