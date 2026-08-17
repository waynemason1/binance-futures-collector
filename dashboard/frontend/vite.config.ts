import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  build: {
    rollupOptions: {
      output: {
        // Split large vendor deps into their own chunks.
        manualChunks: {
          'vendor-react': ['react', 'react-dom'],
          'vendor-query': ['@tanstack/react-query'],
          'vendor-charts': ['recharts', 'lightweight-charts'],
          'vendor-icons': ['lucide-react'],
        }
      }
    },
    chunkSizeWarningLimit: 1000,
  },
  server: {
    host: '0.0.0.0',
    allowedHosts: true,
    port: 3000,
    strictPort: true,  // Fail if port 3000 is in use instead of auto-switching
    headers: {
      'Cache-Control': 'no-store, no-cache, must-revalidate, proxy-revalidate',
      'Pragma': 'no-cache',
      'Expires': '0'
    },
    proxy: {
      '/api': {
        target: 'http://localhost:8000',
        changeOrigin: true
      }
    }
  },
  // Serving the production build (`vite preview`) needs its own proxy block —
  // Vite does not reuse `server.proxy` for preview. Mirror it so the built app
  // reaches the API exactly as the dev server does. Preferred when serving the
  // dashboard over a high-latency link: the bundled build is a handful of
  // requests instead of the dev server's hundreds of per-module fetches.
  preview: {
    host: '0.0.0.0',
    port: 3000,
    strictPort: true,
    allowedHosts: true,
    proxy: {
      '/api': {
        target: 'http://localhost:8000',
        changeOrigin: true
      }
    }
  }
})
