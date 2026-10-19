import { memo, useEffect, useRef } from "react";
import { useDroppable } from "@dnd-kit/core";
import { ChevronRight, MoreHorizontal } from "lucide-react";
import { connectAll, disconnectAll, openMenu, renameGroup, startRenameGroup, toggleGroupCollapsed } from "../../state/actions";
import { isLive, setState, useBulkActions, useStore } from "../../state/store";
import { ConnectIcon, DisconnectIcon } from "../common/SessionIcons";
import type { ConnectionId, Group } from "../../lib/types";
import { containerId } from "./useTabsDnd";

function GroupHeaderInner({ group, ids }: { group: Group; ids: ConnectionId[] }) {
  const renaming = useStore((s) => s.renamingGroupId === group.id);
  const live = useStore((s) => ids.reduce((n, id) => n + (isLive(s.sessions[id]?.status ?? "idle") ? 1 : 0), 0));
  const { canConnect, canDisconnect } = useBulkActions(ids);
  const inputRef = useRef<HTMLInputElement>(null);
  // Collapsed groups still accept drops on the header.
  const { setNodeRef, isOver } = useDroppable({ id: `${containerId(group.id)}:header`, disabled: !group.collapsed });

  useEffect(() => {
    if (renaming) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [renaming]);

  return (
    <div
      ref={setNodeRef}
      className={`group-header ${group.collapsed ? "is-collapsed" : ""} ${isOver ? "is-over" : ""}`}
      onClick={() => !renaming && toggleGroupCollapsed(group.id)}
      onDoubleClick={(e) => {
        e.stopPropagation();
        startRenameGroup(group.id);
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        openMenu({ x: e.clientX, y: e.clientY, target: { kind: "group", id: group.id } });
      }}
    >
      <ChevronRight size={12} className="group-header__chevron" />
      {renaming ? (
        <input
          ref={inputRef}
          className="group-header__input"
          defaultValue={group.name}
          spellCheck={false}
          onClick={(e) => e.stopPropagation()}
          onBlur={(e) => renameGroup(group.id, e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
            if (e.key === "Escape") setState({ renamingGroupId: null });
          }}
        />
      ) : (
        <span className="group-header__name">
          {group.name}
          {group.gateway?.enabled && group.gateway.host && (
            <span className="group-header__badge" title={`RD Gateway: ${group.gateway.host}`}>
              GW
            </span>
          )}
        </span>
      )}
      <span className="group-header__count">{live ? `${live}/${ids.length}` : ids.length}</span>
      <span className="group-header__actions" onClick={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
        {canConnect && (
          <button className="tab__action" title="Connect all in group" aria-label="Connect all in group" onClick={() => connectAll(ids)}>
            <ConnectIcon size={12} />
          </button>
        )}
        {canDisconnect && (
          <button
            className="tab__action"
            title="Disconnect all in group"
            aria-label="Disconnect all in group"
            onClick={() => disconnectAll(ids)}
          >
            <DisconnectIcon size={12} />
          </button>
        )}
        <button
          className="tab__action"
          title="More"
          aria-label="Group actions"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            openMenu({ x: r.left, y: r.bottom + 4, target: { kind: "group", id: group.id } });
          }}
        >
          <MoreHorizontal size={13} />
        </button>
      </span>
    </div>
  );
}

export const GroupHeader = memo(GroupHeaderInner);
