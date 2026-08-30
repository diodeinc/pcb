import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  base: './',
  server: { host: '127.0.0.1', strictPort: true },
  // Local corpus fixtures are for development only, never part of the app build.
  build: { copyPublicDir: false },
  test: { environment: 'jsdom', include: ['src/**/*.test.{ts,tsx}'] },
});
