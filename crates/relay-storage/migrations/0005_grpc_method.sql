-- Which gRPC method a request calls.
--
-- Empty for every other kind. One column rather than a service and a
-- method, because `package.Service/Method` is how the path is spelled
-- and splitting it would mean joining it back on every call.
ALTER TABLE requests ADD COLUMN grpc_method TEXT NOT NULL DEFAULT '';
