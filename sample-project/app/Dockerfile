# ---- build stage ----
# Fully-qualified (docker.io/library/...) rather than a short name: rootless
# Podman doesn't ship with unqualified-search-registries configured by
# default, so "maven:..." fails to resolve on a fresh install.
FROM docker.io/library/maven:3.9-eclipse-temurin-17 AS build
WORKDIR /build

# Cache dependencies in their own layer before copying source.
COPY pom.xml .
RUN mvn -B dependency:go-offline

COPY src ./src
RUN mvn -B package -DskipTests

# ---- runtime stage ----
FROM docker.io/library/eclipse-temurin:17-jre-alpine
WORKDIR /app
COPY --from=build /build/target/*.jar app.jar

EXPOSE 8080
ENTRYPOINT ["java", "-jar", "app.jar"]
