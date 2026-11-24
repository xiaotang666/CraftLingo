import { useCallback, useEffect, useState } from "react";
import { PageHeader } from "../components/PageHeader";
import { SearchInput } from "../components/SearchInput";
import { useDebouncedValue } from "../hooks/useDebouncedValue";
import { glossaryStatus, listGlossaryTerms, type GlossaryTerm } from "../api/tauri";
import { t } from "../i18n/translations";
import type { AppLanguage } from "../types";

interface Props {
  language: AppLanguage;
}

const GLOSSARY_PAGE_SIZE = 200;

export function GlossaryPage({ language }: Props) {
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState<GlossaryTerm[]>([]);
  const [total, setTotal] = useState(0);
  const [files, setFiles] = useState(0);
  const [terms, setTerms] = useState(0);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState("");
  const debounced = useDebouncedValue(query, 300);
  const activeQuery = debounced.trim();

  useEffect(() => {
    glossaryStatus()
      .then((status) => {
        setFiles(status.files);
        setTerms(status.terms);
      })
      .catch(() => {});
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const page = await listGlossaryTerms(activeQuery, 0, GLOSSARY_PAGE_SIZE);
      setEntries(page.entries);
      setTotal(page.total);
    } catch (err) {
      setError(String(err));
      setEntries([]);
      setTotal(0);
    } finally {
      setLoading(false);
    }
  }, [activeQuery]);

  useEffect(() => {
    void load();
  }, [load]);

  const loadMore = useCallback(async () => {
    setLoadingMore(true);
    try {
      const page = await listGlossaryTerms(activeQuery, entries.length, GLOSSARY_PAGE_SIZE);
      setEntries((prev) => [...prev, ...page.entries]);
      setTotal(page.total);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoadingMore(false);
    }
  }, [activeQuery, entries.length]);

  return (
    <section className="page glossary-page">
      <PageHeader
        title={t(language, "nav.glossary")}
        subtitle={t(language, "glossary.subtitle", { files, terms })}
      />
      <div className="dictionary-toolbar">
        <SearchInput
          value={query}
          onChange={setQuery}
          placeholder={t(language, "glossary.searchPlaceholder")}
        />
      </div>
      {error && <div className="alert error">{error}</div>}
      <div className="table-wrapper">
        <table className="data-table">
          <thead>
            <tr>
              <th>{t(language, "dictionary.col.source")}</th>
              <th>{t(language, "dictionary.col.target")}</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((entry, index) => (
              <tr key={`${index}-${entry.source}`}>
                <td>{entry.source}</td>
                <td>{entry.target}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {loading && <div className="empty-state">{t(language, "common.loading")}</div>}
      {!loading && entries.length === 0 && (
        <div className="empty-state">{t(language, "glossary.empty")}</div>
      )}
      {!loading && entries.length < total && (
        <div className="load-more-row">
          <button className="ghost-button" type="button" onClick={loadMore} disabled={loadingMore}>
            {t(language, "common.loadMore")}
          </button>
        </div>
      )}
    </section>
  );
}
