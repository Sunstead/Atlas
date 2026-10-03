import { File, FileCode, FileText, Film, Image, Music, type LucideProps } from 'lucide-react';

const CODE = /\/(json|xml|javascript|typescript|x-sh|x-python|x-rust|toml|yaml)/;

/** An icon for an item, by its MIME type. */
export function ItemIcon({ mime, ...props }: { mime: string | null | undefined } & LucideProps) {
  const m = mime ?? '';
  if (m.startsWith('image/')) return <Image {...props} />;
  if (m.startsWith('video/')) return <Film {...props} />;
  if (m.startsWith('audio/')) return <Music {...props} />;
  if (CODE.test(m)) return <FileCode {...props} />;
  if (m.startsWith('text/') || m === 'application/pdf') return <FileText {...props} />;
  return <File {...props} />;
}
