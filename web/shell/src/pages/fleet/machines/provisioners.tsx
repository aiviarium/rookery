// The provisioners: engines a plugin drives that make machines, which become
// hosts. They are a tab of the hosts page, since what they make is hosts.
import { For, Show, createEffect, createSignal } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { ProvisionerEntry, ProvisionerType } from "@rookery/proto";
import { Badge, Button, Card, EmptyState, ListRow, RowList, Skeleton, State } from "@aiviarium/nocturne";

import { createAction } from "../../../parts/action.ts";
import { ConfirmDialog } from "../../../parts/dialogs.tsx";
import { RowActions } from "../../../parts/row-actions.tsx";
import { useShell } from "../../../shell.tsx";
import { jobPath } from "../jobs/rows.tsx";
import { NewProvisioner, RunProvisionerAction, engineName } from "./dialogs.tsx";

type Health = Record<string, { up: boolean; message: string }>;

/** ProvisionersPanel lists the provisioners, adds them and runs their actions. */
export function ProvisionersPanel(props: {
  entries: readonly ProvisionerEntry[] | undefined;
  types: readonly ProvisionerType[];
  hosts: readonly string[];
  adding: boolean;
  onAdd: () => void;
  onCloseAdd: () => void;
  onChanged: () => void;
  onNewMachine: (entry: string) => void;
}): JSX.Element {
  const shell = useShell();
  const [health, setHealth] = createSignal<Health>({});
  const [acting, setActing] = createSignal<ProvisionerEntry>();
  const [removing, setRemoving] = createSignal<ProvisionerEntry>();
  const rows = () => props.entries ?? [];
  const typeOf = (entry: ProvisionerEntry) => props.types.find((one) => one.id === entry.type);
  const makesMachines = (entry: ProvisionerEntry) => typeOf(entry)?.kinds.some((kind) => kind.machine) ?? false;

  // Each engine's health, read again when the entries change.
  createEffect(
    () => rows().map((entry) => entry.name),
    (names) => {
      let stale = false;
      void Promise.all(
        names.map(async (name) => {
          const status = await shell.rookery.provisioners.status({ provisioner: name }).catch(() => undefined);
          return [name, { up: status?.up ?? false, message: status?.message ?? "" }] as const;
        }),
      ).then((pairs) => {
        if (!stale) setHealth(Object.fromEntries(pairs));
      });
      return () => {
        stale = true;
      };
    },
  );

  const remove = createAction(async () => {
    const entry = removing();
    if (entry !== undefined) await shell.rookery.provisioners.remove({ provisioner: entry.name });
    props.onChanged();
  }, { done: "Provisioner removed" });

  const menu = (entry: ProvisionerEntry) => [
    ...(makesMachines(entry) ? [{ id: "machine", label: "New machine", icon: "plus" as const }] : []),
    ...((typeOf(entry)?.actions.length ?? 0) > 0 ? [{ id: "action", label: "Run an action", icon: "play" as const }] : []),
    { type: "separator" as const },
    { id: "remove", label: "Remove", icon: "trash" as const, danger: true },
  ];
  const choose = (entry: ProvisionerEntry, id: string) => {
    if (id === "machine") props.onNewMachine(entry.name);
    if (id === "action") setActing(entry);
    if (id === "remove") setRemoving(entry);
  };

  return (
    <>
      <Show when={props.entries} fallback={<Skeleton width="50%" />}>
        <Show
          when={rows().length > 0}
          fallback={
            <Card>
              <EmptyState icon="layers" title="No provisioners">
                <Button icon="plus" onClick={props.onAdd}>
                  Add provisioner
                </Button>
              </EmptyState>
            </Card>
          }
        >
          <Card padding="none">
            <RowList label="Provisioners">
              <For each={rows()} keyed={(entry) => entry.name}>
                {(entry) => {
                  const up = () => health()[entry().name];
                  return (
                    <ListRow
                      data-provisioner={entry().name}
                      icon="layers"
                      title={entry().name}
                      secondary={<span class="nc-mono">{`${typeOf(entry())?.label || entry().type} · ${engineName(entry())}`}</span>}
                      detail={up() !== undefined && !up()!.up && up()!.message !== "" ? up()!.message : undefined}
                      status={<Show when={up() !== undefined && !up()!.up}><State status="error">Down</State></Show>}
                      meta={
                        <span class="nc-row nc-row--tight">
                          <For each={typeOf(entry())?.kinds ?? []}>{(kind) => <Badge tone="outline">{kind.label || kind.id}</Badge>}</For>
                        </span>
                      }
                      menu={menu(entry())}
                      menuLabel={`Actions for ${entry().name}`}
                      onMenuSelect={(id) => choose(entry(), id)}
                      persistentActions={
                        <RowActions label={`Actions for ${entry().name}`} items={menu(entry())} onSelect={(id) => choose(entry(), id)} />
                      }
                    />
                  );
                }}
              </For>
            </RowList>
          </Card>
        </Show>
      </Show>

      <Show when={props.adding}>
        <NewProvisioner
          types={props.types}
          hosts={props.hosts}
          onClose={props.onCloseAdd}
          onAdded={() => {
            props.onCloseAdd();
            props.onChanged();
          }}
        />
      </Show>
      <Show when={acting()} keyed>
        {(entry) => (
          <RunProvisionerAction
            entry={entry}
            type={typeOf(entry)}
            onClose={() => setActing(undefined)}
            onStarted={(job) => {
              setActing(undefined);
              shell.navigate(jobPath(job));
            }}
          />
        )}
      </Show>
      <ConfirmDialog
        open={removing() !== undefined}
        title={`Remove ${removing()?.name ?? "the provisioner"}?`}
        description="Its machines stay as hosts."
        confirm="Remove provisioner"
        danger
        action={remove}
        onClose={() => setRemoving(undefined)}
      />
    </>
  );
}
