-- name: CreateGroup :one
INSERT INTO rookery.groups (id, name)
VALUES ($1, $2)
RETURNING *;

-- name: GetGroup :one
SELECT *
FROM rookery.groups
WHERE id = $1;

-- name: GetGroupByName :one
SELECT *
FROM rookery.groups
WHERE name = $1;

-- name: ListGroups :many
SELECT *
FROM rookery.groups
WHERE id > $1
ORDER BY id
LIMIT $2;

-- name: RenameGroup :one
UPDATE rookery.groups
SET name = $2
WHERE id = $1
RETURNING *;

-- name: DeleteGroup :execrows
DELETE FROM rookery.groups
WHERE id = $1;

-- name: AddGroupMember :exec
INSERT INTO rookery.group_members (group_id, user_id, source)
VALUES ($1, $2, $3)
ON CONFLICT DO NOTHING;

-- name: RemoveGroupMember :execrows
DELETE FROM rookery.group_members
WHERE group_id = $1
  AND user_id = $2;

-- name: ListGroupMembers :many
SELECT u.*
FROM rookery.users u
JOIN rookery.group_members m ON m.user_id = u.id
WHERE m.group_id = $1
ORDER BY u.email;

-- name: ListGroupsForUser :many
SELECT g.*
FROM rookery.groups g
JOIN rookery.group_members m ON m.group_id = g.id
WHERE m.user_id = $1
ORDER BY g.name;

-- name: BindGroupClaim :exec
INSERT INTO rookery.group_claims (group_id, claim, value)
VALUES ($1, $2, $3)
ON CONFLICT DO NOTHING;

-- name: UnbindGroupClaim :execrows
DELETE FROM rookery.group_claims
WHERE group_id = $1
  AND claim = $2
  AND value = $3;

-- name: ListGroupClaims :many
SELECT *
FROM rookery.group_claims
WHERE group_id = $1
ORDER BY claim, value;

-- name: ListGroupsForClaim :many
SELECT DISTINCT group_id
FROM rookery.group_claims
WHERE claim = $1
  AND value = ANY($2::text[]);

-- name: DeleteClaimMemberships :exec
DELETE FROM rookery.group_members
WHERE user_id = $1
  AND source = 'claim';

-- name: AddGroupMemberManual :exec
INSERT INTO rookery.group_members (group_id, user_id, source)
VALUES ($1, $2, 'manual')
ON CONFLICT (group_id, user_id) DO UPDATE
SET source = 'manual';
