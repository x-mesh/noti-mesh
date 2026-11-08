// Runs before the stylesheets load, so that a dark page does not flash white first.
(() => {
  const key = 'noti-mesh-theme';
  const media = matchMedia('(prefers-color-scheme: dark)');
  const read = () => {
    try { return localStorage.getItem(key) || 'system'; } catch { return 'system'; }
  };
  const apply = (choice) => {
    const dark = choice === 'dark' || (choice === 'system' && media.matches);
    document.documentElement.dataset.jkScheme = dark ? 'dark' : 'light';
  };
  apply(read());
  media.addEventListener('change', () => apply(read()));
  window.notiTheme = {
    get: read,
    set(choice) {
      try { localStorage.setItem(key, choice); } catch {}
      apply(choice);
    },
  };
})();
