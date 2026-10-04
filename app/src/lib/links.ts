/**
 * "Open in app" links. Web apps open in a new tab; app links
 * (`solstice://open?...`) hand off to the installed app, which would leave
 * an empty tab behind, so they open in place.
 */
export function isWebLink(url: string): boolean {
  return /^https?:\/\//i.test(url);
}

/** Props for an `<a>` that opens `url` the right way. */
export function linkProps(url: string): { href: string; target?: string; rel?: string } {
  return isWebLink(url) ? { href: url, target: '_blank', rel: 'noreferrer' } : { href: url };
}

export function openLink(url: string) {
  if (isWebLink(url)) window.open(url, '_blank', 'noreferrer');
  else window.location.assign(url);
}
