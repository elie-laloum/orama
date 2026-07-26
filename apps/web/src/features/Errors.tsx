import { useQuery } from "@tanstack/react-query";

import { api } from "@/api/client";
import {
  ConfidenceBadge, Empty, Failed, Loading, Panel, Pill, SeverityBadge,
} from "@/components/ui";
import { ago, compact } from "@/domain/format";
import { Page } from "@/features/Page";

/**
 * Findings grouped by rule, worst first. Grouping is the point: twenty-two
 * identical tool failures are one problem, not twenty-two.
 */
export function Errors() {
  const query = useQuery({ queryKey: ["errors"], queryFn: api.errors });

  return (
    <Page title="Errors" subtitle="Findings grouped by rule — each explains itself">
      {query.isLoading ? <Loading /> : query.error ? <Failed error={query.error} /> :
       query.data?.groups.length ? (
        <div className="space-y-2">
          {query.data.groups.map((group) => (
            <Panel key={group.rule_id}>
              <div className="p-3">
                <div className="flex flex-wrap items-center gap-2">
                  <SeverityBadge severity={group.severity} />
                  <h3 className="text-sm font-medium">{group.title}</h3>
                  <ConfidenceBadge confidence={group.confidence} />
                  <Pill>{group.category}</Pill>
                  <code className="ml-auto font-mono text-2xs text-faint">{group.rule_id}</code>
                </div>

                <p className="mt-2 text-sm text-muted">{group.sample_summary}</p>

                <dl className="mt-2.5 space-y-1 text-2xs">
                  <Row term="Why" text={group.explanation} />
                  <Row term="Impact" text={group.impact} />
                  <Row term="Do" text={group.recommendation} />
                </dl>

                <div className="mt-2.5 flex flex-wrap items-center gap-3 border-t border-border/50 pt-2 text-2xs text-faint tnum">
                  <span><strong className="text-fg">{compact(group.occurrences)}</strong> occurrences</span>
                  <span>across {compact(group.sessions)} session{group.sessions === 1 ? "" : "s"}</span>
                  <span className="ml-auto">last {ago(group.last_seen)}</span>
                </div>
              </div>
            </Panel>
          ))}
        </div>
      ) : (
        <Empty
          label="Nothing wrong was found"
          hint="Detectors run on every capture; findings will appear here as they occur."
        />
      )}
    </Page>
  );
}

function Row({ term, text }: { term: string; text: string }) {
  return (
    <div className="flex gap-2">
      <dt className="w-12 shrink-0 text-faint">{term}</dt>
      <dd className="min-w-0 flex-1 text-muted">{text}</dd>
    </div>
  );
}
