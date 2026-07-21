# Always start with production / full release and work backwards

- When building software that will be available for download, always start with official **stable** releases (albeit the major and minor number should be zero, e.g. 0.0.1, 0.0.2, etc.).
- When building software that is hosted in the cloud, always start with hosting it directly in production on the real domain name on which real users will use it.
- As the software matures and test users begin to come in, additional controls should be added, such as staging, an **unstable** channel for downloadable software, etc. But always start with the actual thing, then build the controls around that. By the time launch day arrives, there should be enough controls to fix bugs and release new versions without breaking anything.

Use the word **unstable** (not “edge” or “prerelease”) for non-stable downloadable builds. See `continuous-delivery-downloadable` for channel rules.
