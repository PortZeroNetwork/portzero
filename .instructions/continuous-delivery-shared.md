# Continuous Delivery — shared rules

Apply alongside `continuous-delivery-cloud` and/or
`continuous-delivery-downloadable`.

- **Never** enable Github Actions jobs on arbitrary branches; they should only exist on staging or on PRs into staging.
- When we need cloud infrastructure, **always** use Terraform to manage it.
