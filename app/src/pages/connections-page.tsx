import { useState } from 'react';
import { ExternalLink } from 'lucide-react';
import { Button } from '@sunstead/ui/components/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@sunstead/ui/components/card';
import { Input } from '@sunstead/ui/components/input';
import { useConnectionMutations, useConnections, useSourceKinds } from '@/lib/connections';
import { formatWhen } from '@/lib/format';
import { ThemeSelect } from '@/components/theme-select';
import type { ConnectionInfo } from '@/generated/ConnectionInfo';
import type { SourceKindInfo } from '@/generated/SourceKindInfo';

/** `/settings/connections`: the sources Atlas searches for this user. */
export function ConnectionsPage() {
  const kinds = useSourceKinds();
  const connections = useConnections();

  return (
    <div className='mx-auto flex w-full max-w-2xl flex-col gap-4 px-4 py-6'>
      <div>
        <h1 className='font-heading text-xl font-semibold'>Connections</h1>
        <p className='text-sm text-muted-foreground'>The sources Atlas searches for you. Only you can see yours.</p>
      </div>
      {(kinds.isError || connections.isError) && (
        <p className='text-sm text-error'>Couldn't load connections. {(kinds.error ?? connections.error)?.message}</p>
      )}
      {kinds.data?.map((kind) => (
        <KindCard key={kind.kind} kind={kind} connection={connections.data?.find((c) => c.kind === kind.kind)} />
      ))}
      <Card>
        <CardHeader>
          <CardTitle>Appearance</CardTitle>
          <CardDescription>Saved in this browser.</CardDescription>
        </CardHeader>
        <CardContent>
          <ThemeSelect />
        </CardContent>
      </Card>
      <p className='text-xs text-muted-foreground'>
        Tip: press / anywhere to search. In Firefox, open the address bar's search menu and add Atlas to search it straight from
        the address bar.
      </p>
    </div>
  );
}

function KindCard({ kind, connection }: { kind: SourceKindInfo; connection?: ConnectionInfo }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{kind.name}</CardTitle>
        <CardDescription>{kind.description}</CardDescription>
      </CardHeader>
      <CardContent>
        {connection ? (
          <Connected kind={kind} connection={connection} />
        ) : kind.enabled ? (
          <ConnectForm kind={kind} />
        ) : (
          <p className='text-sm text-muted-foreground'>{kind.disabled_reason}</p>
        )}
      </CardContent>
    </Card>
  );
}

function CredentialHelp({ kind }: { kind: SourceKindInfo }) {
  if (!kind.credential) return null;
  return (
    <p className='text-xs text-muted-foreground'>
      {kind.credential.help}{' '}
      {kind.credential.url && (
        <a href={kind.credential.url} target='_blank' rel='noreferrer' className='inline-flex items-center gap-0.5 underline'>
          Open {kind.name}
          <ExternalLink className='size-3' />
        </a>
      )}
    </p>
  );
}

function ConnectForm({ kind }: { kind: SourceKindInfo }) {
  const { create } = useConnectionMutations();
  const [credential, setCredential] = useState('');
  const needs = kind.credential?.required ?? false;

  return (
    <form
      className='flex flex-col gap-2'
      onSubmit={(e) => {
        e.preventDefault();
        create.mutate({ kind: kind.kind, credential: credential.trim() || undefined });
      }}
    >
      {kind.credential && (
        <>
          <label className='text-sm font-medium' htmlFor={`cred-${kind.kind}`}>
            {kind.credential.label}
            {!needs && <span className='font-normal text-muted-foreground'> (optional)</span>}
          </label>
          <Input
            id={`cred-${kind.kind}`}
            type='password'
            autoComplete='off'
            value={credential}
            onChange={(e) => setCredential(e.target.value)}
          />
          <CredentialHelp kind={kind} />
        </>
      )}
      {create.isError && <p className='text-sm text-error'>{create.error.message}</p>}
      <div>
        <Button type='submit' disabled={create.isPending || (needs && !credential.trim())}>
          Connect
        </Button>
      </div>
    </form>
  );
}

function Connected({ kind, connection }: { kind: SourceKindInfo; connection: ConnectionInfo }) {
  const { update, remove, sync } = useConnectionMutations();
  const [replacing, setReplacing] = useState(false);
  const [credential, setCredential] = useState('');
  const error = update.error ?? remove.error ?? sync.error;

  return (
    <div className='flex flex-col gap-3'>
      <p className='text-sm'>
        Connected as <span className='font-medium'>{connection.label}</span>
        {!connection.enabled && <span className='text-muted-foreground'>, paused</span>}
        {kind.credential && (
          <span className='text-muted-foreground'>
            {connection.has_credential ? `. ${kind.credential.label} saved.` : `. No ${kind.credential.label.toLowerCase()} yet.`}
          </span>
        )}
      </p>
      {connection.sync && connection.enabled && <SyncLine sync={connection.sync} />}

      {replacing && kind.credential && (
        <form
          className='flex flex-col gap-2'
          onSubmit={(e) => {
            e.preventDefault();
            update.mutate(
              { id: connection.id, credential: credential.trim() },
              {
                onSuccess: () => {
                  setReplacing(false);
                  setCredential('');
                },
              },
            );
          }}
        >
          <label className='text-sm font-medium' htmlFor={`new-cred-${kind.kind}`}>
            New {kind.credential.label.toLowerCase()}
          </label>
          <Input
            id={`new-cred-${kind.kind}`}
            type='password'
            autoComplete='off'
            autoFocus
            value={credential}
            onChange={(e) => setCredential(e.target.value)}
          />
          <CredentialHelp kind={kind} />
          <div className='flex gap-2'>
            <Button type='submit' disabled={update.isPending || !credential.trim()}>
              Save
            </Button>
            <Button type='button' variant='ghost' onClick={() => setReplacing(false)}>
              Cancel
            </Button>
          </div>
        </form>
      )}

      {error && <p className='text-sm text-error'>{error.message}</p>}

      {!replacing && (
        <div className='flex flex-wrap gap-2'>
          {kind.credential && (
            <Button variant='outline' onClick={() => setReplacing(true)}>
              {connection.has_credential ? `Replace ${kind.credential.label.toLowerCase()}` : `Add ${kind.credential.label.toLowerCase()}`}
            </Button>
          )}
          {connection.sync && connection.enabled && (
            <Button variant='outline' disabled={sync.isPending || connection.sync.running} onClick={() => sync.mutate(connection.id)}>
              Sync now
            </Button>
          )}
          <Button
            variant='outline'
            disabled={update.isPending}
            onClick={() => update.mutate({ id: connection.id, enabled: !connection.enabled })}
          >
            {connection.enabled ? 'Pause' : 'Resume'}
          </Button>
          <Button
            variant='destructive'
            disabled={remove.isPending}
            onClick={() => {
              if (window.confirm(`Disconnect ${kind.name}? Atlas forgets the key and stops searching it.`)) {
                remove.mutate(connection.id);
              }
            }}
          >
            Disconnect
          </Button>
        </div>
      )}
    </div>
  );
}

function SyncLine({ sync }: { sync: NonNullable<ConnectionInfo['sync']> }) {
  const items = `${sync.items.toLocaleString()} ${sync.items === 1 ? 'item' : 'items'}`;
  return (
    <p className='text-sm text-muted-foreground'>
      {sync.running
        ? `Syncing. ${items} so far.`
        : sync.last_synced_at
          ? `${items}, synced ${formatWhen(sync.last_synced_at)}.`
          : 'Not synced yet.'}
      {sync.error && <span className='block text-error'>Last sync failed: {sync.error}</span>}
    </p>
  );
}
