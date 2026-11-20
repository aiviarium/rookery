-- name: FindEntity :one
SELECT uri, kind, title, access FROM rookery.entities WHERE uri = $1;

-- name: FindEntities :many
SELECT uri, kind, title, access FROM rookery.entities WHERE uri = ANY($1::text[]);

-- name: SearchEntities :many
WITH request AS (
    SELECT websearch_to_tsquery('simple', sqlc.arg('text_query')::text) AS text_query,
           nullif(sqlc.arg('vector_query')::text, '')::vector AS vector_query
), scored AS (
    SELECT e.uri, e.kind, e.title, e.access, e.updated_at, e.search @@ r.text_query AS text_match,
           ts_rank(e.search, r.text_query) AS text_score,
           CASE WHEN vector_dims(e.embedding) = vector_dims(r.vector_query)
                THEN 1 - (e.embedding <=> r.vector_query)
           END AS vector_score
    FROM rookery.entities e CROSS JOIN request r
    WHERE e.search @@ r.text_query OR vector_dims(e.embedding) = vector_dims(r.vector_query)
)
SELECT uri, kind, title, access
FROM scored
WHERE text_match OR vector_score IS NOT NULL
ORDER BY text_score + coalesce(vector_score, 0) DESC, updated_at DESC, uri
LIMIT sqlc.arg('row_limit') OFFSET sqlc.arg('row_offset');
