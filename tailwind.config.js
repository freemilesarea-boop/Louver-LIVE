/** @type {import('tailwindcss').Config} */
export default {
  content: ['./apps/desktop/index.html', './apps/desktop/src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        // §61: neutral/dark throughout; LIVE is the only saturated state colour.
        ink: {
          950: '#0B0D10',
          900: '#101317',
          850: '#151920',
          800: '#1B2027',
          700: '#262C35',
          600: '#39414D',
          500: '#5A6472',
          400: '#8A93A1',
          300: '#B6BDC8',
          100: '#E8EBEF',
        },
        live: { DEFAULT: '#E5484D', dim: '#7F2528' },
        ok: { DEFAULT: '#2FB57C', dim: '#1C6B4A' },
        warn: { DEFAULT: '#D9A222', dim: '#7A5B12' },
      },
      fontFamily: {
        sans: ['Inter', 'Pretendard', 'system-ui', '-apple-system', 'Segoe UI', 'sans-serif'],
        mono: ['ui-monospace', 'SFMono-Regular', 'Menlo', 'Consolas', 'monospace'],
      },
    },
  },
  plugins: [],
}
