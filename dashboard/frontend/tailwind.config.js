/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  theme: {
    extend: {
      colors: {
        ink:    '#080A0E',   // page
        panel:  '#0F131A',   // cards
        raised: '#161B24',   // raised surfaces
        line:   '#212834',   // hairlines
        text:   '#EAEDF3',
        muted:  '#98A2B2',
        faint:  '#5C6675',
        // Reserved status (never used as a series hue)
        live:   '#FFB224',   // active / lit lamp
        alert:  '#FB6B7E',   // stale / gap
        good:   '#34E5B0',   // continuous / healthy
        // Single accent (interactive + magnitude ramp anchor)
        accent: {
          DEFAULT: '#2DD4BF',
          soft: '#1B3A3B',
          dim: '#0F6E63',
        },
      },
      fontFamily: {
        display: ['"Space Grotesk"', 'sans-serif'],
        mono: ['"IBM Plex Mono"', 'ui-monospace', 'monospace'],
      },
      letterSpacing: { tightest: '-0.03em' },
      boxShadow: {
        glow: '0 0 24px -6px rgba(45,212,191,0.35)',
        lamp: '0 0 12px rgba(255,178,36,0.55)',
        panel: '0 1px 0 0 rgba(255,255,255,0.03) inset, 0 8px 30px -12px rgba(0,0,0,0.6)',
      },
    },
  },
  plugins: [],
  darkMode: 'class',
}
